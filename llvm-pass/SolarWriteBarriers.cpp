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

// The debug location to stamp on a barrier call inserted next to `Src`. Uses
// `Src`'s own location when it has one; otherwise — when `Src` was synthesized
// by the optimizer with no `!dbg` (e.g. a MemCpyOpt/loop-idiom memcpy) — falls
// back to a line-0 location at the enclosing function's subprogram scope.
// LLVM's verifier requires every inlinable call in a function that carries
// debug info to have a `!dbg`, so an empty location on the barrier call would
// break the module in `-g` (release) builds.
static DebugLoc barrierDebugLoc(Instruction *Src) {
  if (DebugLoc DL = Src->getDebugLoc())
    return DL;
  Function *F = Src->getFunction();
  if (F)
    if (DISubprogram *SP = F->getSubprogram())
      return DILocation::get(F->getContext(), 0, 0, SP);
  return DebugLoc();
}

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
    // Model as `aligned_alloc(align, size)` (non-zeroing): codegen emits an
    // explicit `memset` after every sol_alloc, so the zeroing is a separate
    // DSE-able store rather than baked into the allocator. aligned_alloc is a
    // TLI-recognized allocator (so non-escaping allocations still SROA/elide),
    // but — unlike `malloc` — LLVM has no `aligned_alloc + memset -> calloc`
    // fold, so the `!solar.alloc` metadata survives opt and `raiseGcAlloc` can
    // always recover it. (A `malloc` placeholder gets refolded to `calloc` with
    // the metadata dropped, which would silently bypass the GC.)
    FunctionCallee AlignedAlloc = M.getOrInsertFunction(
        "aligned_alloc", FunctionType::get(PtrTy, {I64, I64}, false));

    // sol_memcpy is a plain copy with no GC side effects; rewrite it to the
    // recognized llvm.memmove intrinsic so the optimizer can DSE copies into
    // dead/elided objects (and treat the args as nocapture/argmem, which a
    // custom call would not be). It must be memMOVE, not memcpy: Solar copy
    // semantics allow the operands to alias (`x = x;`, overlapping slice-range
    // assignment), and sol_memcpy is ptr::copy (memmove) in the runtime —
    // llvm.memcpy's non-overlap requirement would be instant UB. This does not
    // cost elision: MemCpyOpt turns a memmove whose operands provably don't
    // alias (the common fresh-allocation fill) back into memcpy early in the
    // -O3 pipeline, and InstCombine's dead-alloc removal accepts either
    // intrinsic writing into the allocation. Codegen emits sol_memcpy ONLY for
    // pointer-free bytes (GC-pointer words are copied with typed `store ptr`
    // member assignments), so the lowered memmove is tagged `!solar.nobarrier`
    // and solar-write-barriers skips it — a plain-data copy (e.g. `[Uint8]`
    // contents) costs no barrier. Optimizer-synthesized transfers (loop idiom,
    // MemCpyOpt rewrites) carry no tag and stay conservatively instrumented.
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
        CallInst *NC = B.CreateCall(AlignedAlloc, {AlignC, Size});
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
        // non-volatile; sol_memcpy is ptr::copy (memmove), and Solar copies may
        // alias, so memmove is the only sound lowering (see comment above).
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

// Raise surviving malloc+!solar.alloc placeholders back to sol_alloc. Returns
// count raised. The placeholder is `malloc(size)`; the explicit zeroing memset
// (emitted by codegen) lives separately in the IR and is DSE'd or kept by opt.
// If InstCombine folded `malloc + memset` into `calloc` (i.e. the zeroing was
// NOT dead), the explicit memset is gone, so we re-materialize it after the
// raised sol_alloc (which does not zero).
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
        // The fold consumed the zeroing into calloc; sol_alloc won't zero, so
        // re-add the memset (which was demonstrably not dead, hence the fold).
        B.CreateMemSet(NA, ConstantInt::get(I8, 0), Size, MaybeAlign());
      }
      CI->replaceAllUsesWith(NA);
      CI->eraseFromParent();
      ++N;
    }
  }

  // Safety net: a recognized allocator call must never survive in generated
  // code — if a malloc/calloc lost its !solar.alloc metadata under some fold it
  // would link to libc and silently bypass the GC. Fail the build instead.
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

    unsigned NStore = 0, NVec = 0, NMem = 0, NSkipStack = 0, NSkipPlain = 0;

    for (Function &F : M) {
      if (F.isDeclaration())
        continue;
      StringRef Name = F.getName();
      if (!(Name.starts_with("solar_") || Name == "main"))
        continue;

      // Collect first; we insert new calls, so don't mutate while iterating.
      // Residual `sol_memcpy` calls (unlowered builds) need no handling at
      // all: codegen emits sol_memcpy ONLY for pointer-free bytes, so it can
      // never carry a GC pointer.
      SmallVector<StoreInst *, 32> Stores;
      SmallVector<AnyMemTransferInst *, 8> Mems;
      for (Instruction &I : instructions(F)) {
        if (auto *SI = dyn_cast<StoreInst>(&I)) {
          Type *VTy = SI->getValueOperand()->getType();
          // Pointer-typed stores (scalar and vector): codegen emits every
          // GC-pointer copy through a `uint8_t*`-typed member/cast (value
          // typedefs carry real pointer members at their pointer words), so
          // pointer stores reach us as `store ptr` and are instrumented
          // precisely. Stores WIDER than a pointer (i128, <2 x i64>, …) are
          // kept as a conservative safety net: optimizer passes that widen or
          // vectorize adjacent stores (SLP, memcpy lowering of 16-byte fat
          // values through the inlined 128-bit atomics) are type-agnostic and
          // may fold pointer words into them. Plain 8-byte integer/float
          // stores are NOT instrumented — with typed codegen they are data,
          // and shading them would put a barrier on every scalar store.
          if (VTy->isPtrOrPtrVectorTy() || DL.getTypeStoreSize(VTy) > 8)
            Stores.push_back(SI);
        } else if (auto *MT = dyn_cast<AnyMemTransferInst>(&I)) {
          // A memmove lowered from codegen's `sol_memcpy` copies pointer-free
          // bytes by construction (`!solar.nobarrier`) — plain-data copies
          // like `[Uint8]` contents get no barrier. Unmarked transfers
          // (optimizer-synthesized: loop idiom, MemCpyOpt rewrites — which
          // may fuse typed pointer stores) stay conservatively instrumented.
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
        // A constant value (null/undef/global/integer literal) is never a live
        // heap pointer — nothing to shade.
        if (isa<Constant>(Val))
          continue;
        IRBuilder<> B(SI->getNextNode());
        if (Val->getType()->isPointerTy()) {
          // Scalar pointer store: shade the stored value.
          CallInst *C = B.CreateCall(WB, {Dst, Val});
          C->setDebugLoc(barrierDebugLoc(SI));
          ++NStore;
        } else {
          // Vector-of-pointers / wider-than-pointer store (i128, <N x i64>, …):
          // conservatively shade every pointer-sized word of the stored region
          // (can't name the individual lanes cheaply).
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
