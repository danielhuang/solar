// LLVM passes for Solar GC allocation lowering and write barriers.
//
// `solar-specialize-gc-alloc` selects fixed-class allocators for constant
// request sizes and exposes pointer-free copies to LLVM. `solar-write-barriers`
// instruments heap pointer writes after optimization.

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
#include "llvm/Support/MathExtras.h"
#include <algorithm>
#include <set>

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

// Redirect constant-size sol_alloc calls to a fixed-size-class runtime entry
// point. Those entry points are const-generic Rust monomorphizations, so their
// bitmap and arena address calculations are optimized for a constant class.
struct SolarSpecializeGcAlloc : PassInfoMixin<SolarSpecializeGcAlloc> {
  PreservedAnalyses run(Module &M, ModuleAnalysisManager &) {
    Function *SolAlloc = M.getFunction("sol_alloc");
    if (!SolAlloc)
      return PreservedAnalyses::all();

    LLVMContext &Ctx = M.getContext();
    Function *SolMemcpy = M.getFunction("sol_memcpy");

    // These are compiler/runtime ABI helpers, not public runtime symbols.
    // Internalizing all of them lets global DCE discard unused classes.
    for (unsigned Class = 0; Class != 28; ++Class)
      if (Function *F =
              M.getFunction(("sol_alloc_class_" + Twine(Class)).str()))
        F->setLinkage(GlobalValue::InternalLinkage);

    SmallVector<CallInst *, 32> AllocCalls;
    SmallVector<CallInst *, 32> MemcpyCalls;

    for (Function &F : M) {
      if (F.isDeclaration() || !isGeneratedFunc(F))
        continue;
      for (Instruction &I : instructions(F))
        if (auto *CI = dyn_cast<CallInst>(&I)) {
          Function *Callee = CI->getCalledFunction();
          if (Callee == SolAlloc)
            AllocCalls.push_back(CI);
          else if (Callee && Callee == SolMemcpy)
            MemcpyCalls.push_back(CI);
        }

    }

    std::set<uint64_t> Classes;
    unsigned NSpecialized = 0, NDynamic = 0;
    for (CallInst *CI : AllocCalls) {
      auto *Size = dyn_cast<ConstantInt>(CI->getArgOperand(0));
      auto *Align = dyn_cast<ConstantInt>(CI->getArgOperand(1));
      if (!Size || !Align || Size->getValue().getActiveBits() > 64 ||
          Align->getValue().getActiveBits() > 64) {
        ++NDynamic;
        continue;
      }
      uint64_t Bytes = Size->getZExtValue();
      uint64_t Alignment = Align->getZExtValue();
      uint64_t Need = std::max<uint64_t>({Bytes, Alignment, 8});
      if (Need > (UINT64_C(1) << 30)) {
        ++NDynamic;
        continue;
      }
      uint64_t Class = Log2_64_Ceil(Need) - 3;
      Function *ClassAlloc =
          M.getFunction(("sol_alloc_class_" + Twine(Class)).str());
      if (!ClassAlloc)
        report_fatal_error("missing fixed-class allocator entry point");
      CI->setCalledFunction(ClassAlloc);
      Classes.insert(Class);
      ++NSpecialized;
    }

    // Generated sol_memcpy calls are overlap-safe and pointer-free. Lower them
    // to tagged memmoves so LLVM can optimize them without adding barriers.
    unsigned NMemcpy = 0;
    for (CallInst *CI : MemcpyCalls) {
      Value *Dst = CI->getArgOperand(0);
      Value *Src = CI->getArgOperand(1);
      Value *Size = CI->getArgOperand(2);
      IRBuilder<> B(CI);
      CallInst *MC =
          B.CreateMemMove(Dst, MaybeAlign(), Src, MaybeAlign(), Size);
      MC->setDebugLoc(CI->getDebugLoc());
      MC->setMetadata("solar.nobarrier", MDNode::get(Ctx, {}));
      CI->eraseFromParent();
      ++NMemcpy;
    }

    if (NSpecialized || NDynamic || NMemcpy)
      errs() << "solar-specialize-gc-alloc: " << NSpecialized
             << " constant-size calls across " << Classes.size() << " classes, "
             << NDynamic << " dynamic-size calls, " << NMemcpy
             << " sol_memcpy -> llvm.memmove\n";
    return (NSpecialized || NMemcpy) ? PreservedAnalyses::none()
                                     : PreservedAnalyses::all();
  }

  static bool isRequired() { return true; }
};

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

    return (NStore || NVec || NMem) ? PreservedAnalyses::none()
                                     : PreservedAnalyses::all();
  }

  // Barriers remain mandatory for `optnone` functions.
  static bool isRequired() { return true; }
};

// Insert checks before every memory operation emitted in generated Solar
// functions. The runtime helper ignores non-arena ranges and rejects any arena
// slot whose allocation bit was cleared by the sweeper.
struct SolarGcSanitize : PassInfoMixin<SolarGcSanitize> {
  PreservedAnalyses run(Module &M, ModuleAnalysisManager &) {
    LLVMContext &Ctx = M.getContext();
    Type *VoidTy = Type::getVoidTy(Ctx);
    Type *I64 = Type::getInt64Ty(Ctx);
    PointerType *PtrTy = PointerType::getUnqual(Ctx);
    const DataLayout &DL = M.getDataLayout();
    FunctionCallee Check = M.getOrInsertFunction(
        "sol_gc_san_check", FunctionType::get(VoidTy, {PtrTy, I64}, false));

    unsigned NChecks = 0;
    auto EmitCheck = [&](Instruction *At, Value *Ptr, Value *Size) {
      IRBuilder<> B(At);
      Value *Size64 = B.CreateZExtOrTrunc(Size, I64);
      CallInst *C = B.CreateCall(Check, {Ptr, Size64});
      C->setDebugLoc(barrierDebugLoc(At));
      ++NChecks;
    };
    auto EmitFixedCheck = [&](Instruction *At, Value *Ptr, Type *AccessTy) {
      uint64_t Size = DL.getTypeStoreSize(AccessTy);
      EmitCheck(At, Ptr, ConstantInt::get(I64, Size));
    };

    for (Function &F : M) {
      if (F.isDeclaration() || !isGeneratedFunc(F))
        continue;

      SmallVector<LoadInst *, 32> Loads;
      SmallVector<StoreInst *, 32> Stores;
      SmallVector<AtomicRMWInst *, 8> RMWs;
      SmallVector<AtomicCmpXchgInst *, 8> CmpXchgs;
      SmallVector<AnyMemTransferInst *, 8> Transfers;
      SmallVector<MemSetInst *, 8> Sets;
      for (Instruction &I : instructions(F)) {
        if (auto *LI = dyn_cast<LoadInst>(&I))
          Loads.push_back(LI);
        else if (auto *SI = dyn_cast<StoreInst>(&I))
          Stores.push_back(SI);
        else if (auto *RMW = dyn_cast<AtomicRMWInst>(&I))
          RMWs.push_back(RMW);
        else if (auto *CX = dyn_cast<AtomicCmpXchgInst>(&I))
          CmpXchgs.push_back(CX);
        else if (auto *MT = dyn_cast<AnyMemTransferInst>(&I))
          Transfers.push_back(MT);
        else if (auto *MS = dyn_cast<MemSetInst>(&I))
          Sets.push_back(MS);
      }

      for (LoadInst *LI : Loads)
        EmitFixedCheck(LI, LI->getPointerOperand(), LI->getType());
      for (StoreInst *SI : Stores)
        EmitFixedCheck(SI, SI->getPointerOperand(),
                       SI->getValueOperand()->getType());
      for (AtomicRMWInst *RMW : RMWs)
        EmitFixedCheck(RMW, RMW->getPointerOperand(),
                       RMW->getValOperand()->getType());
      for (AtomicCmpXchgInst *CX : CmpXchgs)
        EmitFixedCheck(CX, CX->getPointerOperand(),
                       CX->getCompareOperand()->getType());
      for (AnyMemTransferInst *MT : Transfers) {
        EmitCheck(MT, MT->getRawSource(), MT->getLength());
        EmitCheck(MT, MT->getRawDest(), MT->getLength());
      }
      for (MemSetInst *MS : Sets)
        EmitCheck(MS, MS->getRawDest(), MS->getLength());
    }

    return NChecks ? PreservedAnalyses::none() : PreservedAnalyses::all();
  }

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
                  if (Name == "solar-specialize-gc-alloc") {
                    MPM.addPass(SolarSpecializeGcAlloc());
                    return true;
                  }
                  if (Name == "solar-gc-sanitize") {
                    MPM.addPass(SolarGcSanitize());
                    return true;
                  }
                  return false;
                });
          }};
}
