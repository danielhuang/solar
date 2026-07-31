// LLVM passes for Solar GC allocation lowering and write barriers.
//
// `solar-lower-gc-alloc` exposes allocations and pointer-free copies to LLVM
// while retaining the metadata needed to restore surviving allocations.
// `solar-write-barriers` restores them and instruments heap pointer writes.

#include "llvm/Analysis/ValueTracking.h"
#include "llvm/IR/Constants.h"
#include "llvm/IR/DebugInfoMetadata.h"
#include "llvm/IR/IRBuilder.h"
#include "llvm/IR/InstIterator.h"
#include "llvm/IR/Instructions.h"
#include "llvm/IR/IntrinsicInst.h"
#include "llvm/IR/Metadata.h"
#include "llvm/IR/Module.h"
#include "llvm/IR/PassManager.h"
#include "llvm/Passes/PassBuilder.h"
#include "llvm/Plugins/PassPlugin.h"
#include "llvm/Support/ErrorHandling.h"
#include "llvm/Transforms/Utils/ModuleUtils.h"

using namespace llvm;

namespace {

// LLVM requires inserted calls in debug functions to carry a location.
static DebugLoc barrierDebugLoc(Instruction *Src) {
  if (DebugLoc DL = Src->getDebugLoc())
    return DL;
  Function *F = Src->getFunction();
  if (F)
    if (DISubprogram *SP = F->getSubprogram())
      return DILocation::get(F->getContext(), 0, 0, SP);
  return DebugLoc();
}

// Runtime functions are outside the generated `solar_*` and `main` functions.
bool isGeneratedFunc(const Function &F) {
  StringRef N = F.getName();
  return N.starts_with("solar_") || N == "main";
}

bool isStackOrGlobalDest(Value *Dst) {
  const Value *Base = getUnderlyingObject(Dst);
  return isa<AllocaInst>(Base) || isa<GlobalValue>(Base);
}

// Exposes GC allocations to LLVM without losing their collector metadata.
struct SolarLowerGcAlloc : PassInfoMixin<SolarLowerGcAlloc> {
  PreservedAnalyses run(Module &M, ModuleAnalysisManager &) {
    Function *SolAlloc = M.getFunction("sol_alloc");
    if (!SolAlloc)
      return PreservedAnalyses::all();

    LLVMContext &Ctx = M.getContext();
    Type *I64 = Type::getInt64Ty(Ctx);
    PointerType *PtrTy = PointerType::getUnqual(Ctx);
    // `aligned_alloc` is recognized by LLVM, while its separate zeroing memset
    // remains removable and its call metadata survives allocation folding.
    FunctionCallee AlignedAlloc = M.getOrInsertFunction(
        "aligned_alloc", FunctionType::get(PtrTy, {I64, I64}, false));

    // Generated `sol_memcpy` calls are overlap-safe and pointer-free. Lower
    // them to tagged memmoves so LLVM can optimize them without adding barriers.
    Function *SolMemcpy = M.getFunction("sol_memcpy");

    SmallVector<GlobalValue *, 8> MarkFns;
    SmallPtrSet<GlobalValue *, 8> SeenMarkFns;
    unsigned N = 0, Skipped = 0, NMemcpy = 0;

    for (Function &F : M) {
      if (F.isDeclaration() || !isGeneratedFunc(F))
        continue;
      SmallVector<CallInst *, 16> AllocCalls;
      SmallVector<CallInst *, 16> MemcpyCalls;
      for (Instruction &I : instructions(F))
        if (auto *CI = dyn_cast<CallInst>(&I)) {
          Function *Callee = CI->getCalledFunction();
          if (Callee == SolAlloc)
            AllocCalls.push_back(CI);
          else if (Callee && Callee == SolMemcpy)
            MemcpyCalls.push_back(CI);
        }

      for (CallInst *CI : AllocCalls) {
        Value *Size = CI->getArgOperand(0);
        auto *AlignC = dyn_cast<ConstantInt>(CI->getArgOperand(1));
        auto *MarkC = dyn_cast<Constant>(CI->getArgOperand(2));
        // Calls with dynamic collector metadata remain valid but non-elidable.
        if (!AlignC || !MarkC) {
          ++Skipped;
          continue;
        }
        IRBuilder<> B(CI);
        CallInst *NC = B.CreateCall(AlignedAlloc, {AlignC, Size});
        // Tail merging may discard the collector metadata.
        NC->addFnAttr(Attribute::NoMerge);
        NC->setDebugLoc(CI->getDebugLoc());
        Metadata *Ops[] = {ConstantAsMetadata::get(AlignC),
                           ConstantAsMetadata::get(MarkC)};
        NC->setMetadata("solar.alloc", MDNode::get(Ctx, Ops));
        CI->replaceAllUsesWith(NC);
        CI->eraseFromParent();
        if (auto *MF = dyn_cast<Function>(MarkC))
          if (SeenMarkFns.insert(MF).second)
            MarkFns.push_back(MF);
        ++N;
      }

      for (CallInst *CI : MemcpyCalls) {
        Value *Dst = CI->getArgOperand(0);
        Value *Src = CI->getArgOperand(1);
        Value *Size = CI->getArgOperand(2);
        IRBuilder<> B(CI);
        // Solar copies may alias, so this must remain a memmove.
        CallInst *MC =
            B.CreateMemMove(Dst, MaybeAlign(), Src, MaybeAlign(), Size);
        MC->setDebugLoc(CI->getDebugLoc());
        MC->setMetadata("solar.nobarrier", MDNode::get(Ctx, {}));
        CI->eraseFromParent();
        ++NMemcpy;
      }
    }

    // Keep the mark functions alive through opt's globaldce: after lowering
    // their only references are in metadata, which does not count as a use.
    if (!MarkFns.empty())
      appendToCompilerUsed(M, MarkFns);

    if (N || Skipped || NMemcpy)
      errs() << "solar-lower-gc-alloc: " << N << " sol_alloc -> aligned_alloc, "
             << Skipped << " left (non-constant align/mark), " << NMemcpy
             << " sol_memcpy -> llvm.memmove\n";
    return (N || NMemcpy) ? PreservedAnalyses::none() : PreservedAnalyses::all();
  }

  static bool isRequired() { return true; }
};

// Restores tagged allocator calls to `sol_alloc`, including folded zeroing.
unsigned raiseGcAlloc(Module &M) {
  LLVMContext &Ctx = M.getContext();
  Type *I8 = Type::getInt8Ty(Ctx);
  Type *I64 = Type::getInt64Ty(Ctx);
  PointerType *PtrTy = PointerType::getUnqual(Ctx);
  FunctionCallee SolAlloc = M.getOrInsertFunction(
      "sol_alloc", FunctionType::get(PtrTy, {I64, I64, PtrTy}, false));

  unsigned N = 0;
  for (Function &F : M) {
    if (F.isDeclaration() || !isGeneratedFunc(F))
      continue;
    SmallVector<CallInst *, 16> Calls;
    for (Instruction &I : instructions(F))
      if (auto *CI = dyn_cast<CallInst>(&I))
        if (CI->getMetadata("solar.alloc"))
          Calls.push_back(CI);

    for (CallInst *CI : Calls) {
      MDNode *MD = CI->getMetadata("solar.alloc");
      auto *Align = mdconst::extract<ConstantInt>(MD->getOperand(0));
      auto *Mark = mdconst::extract<Constant>(MD->getOperand(1));
      Function *Callee = CI->getCalledFunction();
      StringRef CN = Callee ? Callee->getName() : "";
      bool IsCalloc = CN == "calloc";
      // malloc(size) -> arg0; aligned_alloc(align,size)/calloc(1,size) -> arg1.
      Value *Size = CI->getArgOperand(CN == "malloc" ? 0 : 1);
      IRBuilder<> B(CI);
      CallInst *NA = B.CreateCall(SolAlloc, {Size, Align, Mark});
      NA->setDebugLoc(CI->getDebugLoc());
      if (IsCalloc) {
        // Restore zeroing consumed by a calloc fold.
        B.CreateMemSet(NA, ConstantInt::get(I8, 0), Size, MaybeAlign());
      }
      CI->replaceAllUsesWith(NA);
      CI->eraseFromParent();
      ++N;
    }
  }

  // A surviving libc allocator would silently bypass the collector.
  for (Function &F : M) {
    if (F.isDeclaration() || !isGeneratedFunc(F))
      continue;
    for (Instruction &I : instructions(F))
      if (auto *CI = dyn_cast<CallInst>(&I))
        if (Function *C = CI->getCalledFunction()) {
          StringRef NM = C->getName();
          if (NM == "malloc" || NM == "calloc" || NM == "aligned_alloc")
            report_fatal_error("solar-write-barriers: un-raised allocator call "
                               "in generated code (lost !solar.alloc metadata)");
        }
  }

  if (N)
    errs() << "solar-write-barriers: raised " << N
           << " malloc/calloc -> sol_alloc\n";
  return N;
}

struct SolarWriteBarriers : PassInfoMixin<SolarWriteBarriers> {
  PreservedAnalyses run(Module &M, ModuleAnalysisManager &) {
    LLVMContext &Ctx = M.getContext();
    Type *VoidTy = Type::getVoidTy(Ctx);
    Type *I64 = Type::getInt64Ty(Ctx);
    PointerType *PtrTy = PointerType::getUnqual(Ctx);
    const DataLayout &DL = M.getDataLayout();

    // These calls may be declarations when the runtime is linked separately.
    FunctionCallee WB = M.getOrInsertFunction(
        "sol_write_barrier", FunctionType::get(VoidTy, {PtrTy, PtrTy}, false));
    FunctionCallee MemB = M.getOrInsertFunction(
        "sol_gc_memcpy_barrier",
        FunctionType::get(VoidTy, {PtrTy, I64}, false));

    // Restore surviving allocations before instrumenting stores.
    unsigned NRaised = raiseGcAlloc(M);

    unsigned NStore = 0, NVec = 0, NMem = 0, NSkipStack = 0, NSkipPlain = 0;

    for (Function &F : M) {
      if (F.isDeclaration())
        continue;
      StringRef Name = F.getName();
      if (!(Name.starts_with("solar_") || Name == "main"))
        continue;

      // Collect first because instrumentation mutates the instruction list.
      SmallVector<StoreInst *, 32> Stores;
      SmallVector<AnyMemTransferInst *, 8> Mems;
      for (Instruction &I : instructions(F)) {
        if (auto *SI = dyn_cast<StoreInst>(&I)) {
          Type *VTy = SI->getValueOperand()->getType();
          // Pointer stores are precise; wider stores cover optimizer-created
          // aggregates that may contain pointer words.
          if (VTy->isPtrOrPtrVectorTy() || DL.getTypeStoreSize(VTy) > 8)
            Stores.push_back(SI);
        } else if (auto *MT = dyn_cast<AnyMemTransferInst>(&I)) {
          // Tagged transfers are pointer-free; synthesized transfers are not.
          if (MT->getMetadata("solar.nobarrier"))
            ++NSkipPlain;
          else
            Mems.push_back(MT);
        }
      }

      for (StoreInst *SI : Stores) {
        Value *Val = SI->getValueOperand();
        Value *Dst = SI->getPointerOperand();
        if (isStackOrGlobalDest(Dst)) {
          ++NSkipStack;
          continue;
        }
        // Constants cannot name live GC allocations.
        if (isa<Constant>(Val))
          continue;
        IRBuilder<> B(SI->getNextNode());
        if (Val->getType()->isPointerTy()) {
          // Scalar pointer store: shade the stored value.
          CallInst *C = B.CreateCall(WB, {Dst, Val});
          C->setDebugLoc(barrierDebugLoc(SI));
          ++NStore;
        } else {
          // Conservatively shade every word in a wide store.
          uint64_t Sz = DL.getTypeStoreSize(Val->getType());
          CallInst *C = B.CreateCall(MemB, {Dst, ConstantInt::get(I64, Sz)});
          C->setDebugLoc(barrierDebugLoc(SI));
          ++NVec;
        }
      }

      for (AnyMemTransferInst *MT : Mems) {
        Value *Dst = MT->getRawDest();
        if (isStackOrGlobalDest(Dst)) {
          ++NSkipStack;
          continue;
        }
        IRBuilder<> B(MT->getNextNode());
        Value *Len = B.CreateZExtOrTrunc(MT->getLength(), I64);
        CallInst *C = B.CreateCall(MemB, {Dst, Len});
        C->setDebugLoc(barrierDebugLoc(MT));
        ++NMem;
      }

    }
    (void)NSkipPlain;

    return (NRaised || NStore || NVec || NMem) ? PreservedAnalyses::none()
                                               : PreservedAnalyses::all();
  }

  // Barriers remain mandatory for `optnone` functions.
  static bool isRequired() { return true; }
};

} // namespace

extern "C" LLVM_ATTRIBUTE_WEAK ::llvm::PassPluginLibraryInfo
llvmGetPassPluginInfo() {
  return {LLVM_PLUGIN_API_VERSION, "SolarWriteBarriers", "v1",
          [](PassBuilder &PB) {
            PB.registerPipelineParsingCallback(
                [](StringRef Name, ModulePassManager &MPM,
                   ArrayRef<PassBuilder::PipelineElement>) {
                  if (Name == "solar-write-barriers") {
                    MPM.addPass(SolarWriteBarriers());
                    return true;
                  }
                  if (Name == "solar-lower-gc-alloc") {
                    MPM.addPass(SolarLowerGcAlloc());
                    return true;
                  }
                  return false;
                });
          }};
}
