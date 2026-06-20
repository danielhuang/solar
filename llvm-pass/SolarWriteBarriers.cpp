//===- SolarWriteBarriers.cpp - GC barrier + alloc lowering passes -------===//
//
// New-pass-manager module passes for Solar's GC, run via
// `opt -load-pass-plugin=...so -passes=<name>`. They replace the previous
// textual `llvm-dis | sed | llvm-as` rewrites (src/write_barriers.rs).
//
// Two passes, registered by this plugin:
//
//   solar-lower-gc-alloc  (release only, BEFORE opt -O3)
//     Rewrites every `sol_alloc(size, align, mark_fn)` call in generated code
//     into `calloc(1, size)` carrying the (align, mark_fn) pair in `!solar.alloc`
//     instruction metadata. `calloc` is a recognized allocator (TargetLibraryInfo),
//     so opt -O3 can promote non-escaping allocations to the stack / SROA them
//     away and delete dead ones — which it will NOT do for our custom `sol_alloc`,
//     even when stamped with the full malloc attribute set, because the
//     allocation-elimination transforms key on recognized libcalls, not just
//     attributes. `calloc` (not `malloc`) preserves sol_alloc's zeroing semantics
//     so the optimizer never observes uninitialized reads in the interim. The
//     referenced `_mark_*` functions lose their IR uses once lowered, so they are
//     anchored in `llvm.compiler.used` to survive globaldce until raising.
//
//   solar-write-barriers  (debug + release, AFTER opt -O3)
//     First RAISES surviving `calloc(1, size) !solar.alloc` calls back to
//     `sol_alloc(size, align, mark_fn)` (reading the pair from metadata). Then
//     inserts the GC write barriers below. In debug builds nothing was lowered,
//     so the raise step is a no-op.
//
// Doing barriers as a real pass (vs. text) buys robust getUnderlyingObject()
// provenance, correct DebugLocs (so the verifier never strips module DWARF —
// the bug that lost solar-system debug info for samply), and type safety.
//
// What the barrier step instruments (only in generated code: @solar_* / @main):
//   * `store <ptr> %v, ptr %dst`            -> sol_write_barrier(%dst, %v)
//   * `store <N x ptr> %v, ptr %dst`        -> sol_gc_memcpy_barrier(%dst, size)
//   * llvm.memcpy / llvm.memmove to %dst    -> sol_gc_memcpy_barrier(%dst, len)
// Destinations whose underlying object is an alloca (stack) or a global are
// skipped — those roots are rescanned during the STW remark. Stored values that
// are constants (null/undef/globals) are never heap pointers, so skipped.
//
//===----------------------------------------------------------------------===//

#include "llvm/Analysis/ValueTracking.h"
#include "llvm/IR/Constants.h"
#include "llvm/IR/IRBuilder.h"
#include "llvm/IR/InstIterator.h"
#include "llvm/IR/Instructions.h"
#include "llvm/IR/IntrinsicInst.h"
#include "llvm/IR/Metadata.h"
#include "llvm/IR/Module.h"
#include "llvm/IR/PassManager.h"
#include "llvm/Passes/PassBuilder.h"
#include "llvm/Plugins/PassPlugin.h"
#include "llvm/Transforms/Utils/ModuleUtils.h"

using namespace llvm;

namespace {

// Generated Solar code lives in @solar_* functions and @main; the runtime
// (solar-system) lives elsewhere and must never be touched by these passes.
bool isGeneratedFunc(const Function &F) {
  StringRef N = F.getName();
  return N.starts_with("solar_") || N == "main";
}

bool isStackOrGlobalDest(Value *Dst) {
  const Value *Base = getUnderlyingObject(Dst);
  return isa<AllocaInst>(Base) || isa<GlobalValue>(Base);
}

// solar-lower-gc-alloc: sol_alloc(size,align,mark) -> calloc(1,size) + metadata.
struct SolarLowerGcAlloc : PassInfoMixin<SolarLowerGcAlloc> {
  PreservedAnalyses run(Module &M, ModuleAnalysisManager &) {
    Function *SolAlloc = M.getFunction("sol_alloc");
    if (!SolAlloc)
      return PreservedAnalyses::all();

    LLVMContext &Ctx = M.getContext();
    Type *I64 = Type::getInt64Ty(Ctx);
    PointerType *PtrTy = PointerType::getUnqual(Ctx);
    FunctionCallee Calloc = M.getOrInsertFunction(
        "calloc", FunctionType::get(PtrTy, {I64, I64}, false));
    Constant *One = ConstantInt::get(I64, 1);

    // sol_memcpy is a plain copy with no GC side effects; rewrite it to the
    // recognized llvm.memcpy intrinsic so the optimizer can DSE copies into
    // dead/elided objects (and treat the args as nocapture/argmem, which a
    // custom call would not be). The aggregate-copy barrier is re-added by
    // solar-write-barriers on the surviving llvm.memcpy after optimization.
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
        // align and mark_fn are always compile-time constants in generated
        // code; if some call ever isn't, leave it as sol_alloc (still correct,
        // just not elidable).
        if (!AlignC || !MarkC) {
          ++Skipped;
          continue;
        }
        IRBuilder<> B(CI);
        CallInst *NC = B.CreateCall(Calloc, {One, Size});
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
        // non-volatile; sol_memcpy is copy_nonoverlapping, so memcpy is sound.
        CallInst *MC = B.CreateMemCpy(Dst, MaybeAlign(), Src, MaybeAlign(), Size);
        MC->setDebugLoc(CI->getDebugLoc());
        CI->eraseFromParent();
        ++NMemcpy;
      }
    }

    // Keep the mark functions alive through opt's globaldce: after lowering
    // their only references are in metadata, which does not count as a use.
    if (!MarkFns.empty())
      appendToCompilerUsed(M, MarkFns);

    if (N || Skipped || NMemcpy)
      errs() << "solar-lower-gc-alloc: " << N << " sol_alloc -> calloc, "
             << Skipped << " left (non-constant align/mark), " << NMemcpy
             << " sol_memcpy -> llvm.memcpy\n";
    return (N || NMemcpy) ? PreservedAnalyses::none() : PreservedAnalyses::all();
  }

  static bool isRequired() { return true; }
};

// Raise surviving calloc+!solar.alloc back to sol_alloc. Returns count raised.
unsigned raiseGcAlloc(Module &M) {
  LLVMContext &Ctx = M.getContext();
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
      Value *Size = CI->getArgOperand(1); // calloc(1, size)
      IRBuilder<> B(CI);
      CallInst *NA = B.CreateCall(SolAlloc, {Size, Align, Mark});
      NA->setDebugLoc(CI->getDebugLoc());
      CI->replaceAllUsesWith(NA);
      CI->eraseFromParent();
      ++N;
    }
  }
  if (N)
    errs() << "solar-write-barriers: raised " << N
           << " calloc -> sol_alloc\n";
  return N;
}

struct SolarWriteBarriers : PassInfoMixin<SolarWriteBarriers> {
  PreservedAnalyses run(Module &M, ModuleAnalysisManager &) {
    LLVMContext &Ctx = M.getContext();
    Type *VoidTy = Type::getVoidTy(Ctx);
    Type *I64 = Type::getInt64Ty(Ctx);
    PointerType *PtrTy = PointerType::getUnqual(Ctx);
    const DataLayout &DL = M.getDataLayout();

    // getOrInsertFunction creates a declaration if the runtime definition is
    // not present in this module (e.g. the debug single-module build).
    FunctionCallee WB = M.getOrInsertFunction(
        "sol_write_barrier", FunctionType::get(VoidTy, {PtrTy, PtrTy}, false));
    FunctionCallee MemB = M.getOrInsertFunction(
        "sol_gc_memcpy_barrier",
        FunctionType::get(VoidTy, {PtrTy, I64}, false));

    // Raise any calloc placeholders left by solar-lower-gc-alloc back to
    // sol_alloc before instrumenting their stores (no-op in debug builds).
    unsigned NRaised = raiseGcAlloc(M);

    // In debug builds (no solar-lower-gc-alloc pass) aggregate copies are still
    // direct sol_memcpy calls; instrument those too. In release they were
    // already turned into llvm.memcpy by the lowering pass and are handled as
    // AnyMemTransferInst below.
    Function *SolMemcpy = M.getFunction("sol_memcpy");

    unsigned NStore = 0, NVec = 0, NMem = 0, NSkipStack = 0;

    for (Function &F : M) {
      if (F.isDeclaration())
        continue;
      StringRef Name = F.getName();
      if (!(Name.starts_with("solar_") || Name == "main"))
        continue;

      // Collect first; we insert new calls, so don't mutate while iterating.
      SmallVector<StoreInst *, 32> Stores;
      SmallVector<AnyMemTransferInst *, 8> Mems;
      SmallVector<CallInst *, 8> MemcpyCalls;
      for (Instruction &I : instructions(F)) {
        if (auto *SI = dyn_cast<StoreInst>(&I)) {
          if (SI->getValueOperand()->getType()->isPtrOrPtrVectorTy())
            Stores.push_back(SI);
        } else if (auto *MT = dyn_cast<AnyMemTransferInst>(&I)) {
          Mems.push_back(MT);
        } else if (auto *CI = dyn_cast<CallInst>(&I)) {
          if (SolMemcpy && CI->getCalledFunction() == SolMemcpy)
            MemcpyCalls.push_back(CI);
        }
      }

      for (StoreInst *SI : Stores) {
        Value *Val = SI->getValueOperand();
        Value *Dst = SI->getPointerOperand();
        if (isStackOrGlobalDest(Dst)) {
          ++NSkipStack;
          continue;
        }
        IRBuilder<> B(SI->getNextNode());
        if (Val->getType()->isPointerTy()) {
          // Scalar pointer store: shade the stored value, unless it's a
          // constant (null/undef/global) which is never a live heap pointer.
          if (isa<Constant>(Val))
            continue;
          CallInst *C = B.CreateCall(WB, {Dst, Val});
          C->setDebugLoc(SI->getDebugLoc());
          ++NStore;
        } else {
          // Vector-of-pointers store: conservatively shade every pointer-sized
          // word of the stored region (can't name the individual lanes cheaply).
          uint64_t Sz = DL.getTypeStoreSize(Val->getType());
          CallInst *C = B.CreateCall(MemB, {Dst, ConstantInt::get(I64, Sz)});
          C->setDebugLoc(SI->getDebugLoc());
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
        C->setDebugLoc(MT->getDebugLoc());
        ++NMem;
      }

      for (CallInst *CI : MemcpyCalls) {
        Value *Dst = CI->getArgOperand(0); // sol_memcpy(dst, src, size)
        if (isStackOrGlobalDest(Dst)) {
          ++NSkipStack;
          continue;
        }
        IRBuilder<> B(CI->getNextNode());
        Value *Len = B.CreateZExtOrTrunc(CI->getArgOperand(2), I64);
        CallInst *C = B.CreateCall(MemB, {Dst, Len});
        C->setDebugLoc(CI->getDebugLoc());
        ++NMem;
      }
    }

    return (NRaised || NStore || NVec || NMem) ? PreservedAnalyses::none()
                                               : PreservedAnalyses::all();
  }

  // Run even on optnone functions (generated code shouldn't be optnone, but be
  // safe — barriers are mandatory for correctness).
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
