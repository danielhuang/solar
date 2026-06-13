//===- SolarWriteBarriers.cpp - GC write barrier insertion pass ----------===//
//
// A new-pass-manager module pass that inserts Solar's GC write barriers, run
// via `opt -load-pass-plugin=...so -passes=solar-write-barriers`. It replaces
// the previous textual `llvm-dis | sed | llvm-as` rewrite (src/write_barriers.rs).
//
// Doing it as a real pass buys:
//   * robust provenance: getUnderlyingObject() sees through GEP/bitcast/phi to
//     decide stack-vs-heap, instead of a fragile textual GEP-chain fixpoint;
//   * correct debug locations: the inserted call inherits the store's DebugLoc
//     via the IR API, so the verifier never strips module debug info (the bug
//     that lost solar-system DWARF for samply);
//   * type safety: no string parsing of `store`/`llvm.memcpy` lines.
//
// What it instruments (only in generated Solar code: @solar_* and @main):
//   * `store <ptr> %v, ptr %dst`            -> sol_write_barrier(%dst, %v)
//   * `store <N x ptr> %v, ptr %dst`        -> sol_gc_memcpy_barrier(%dst, size)
//   * llvm.memcpy / llvm.memmove to %dst    -> sol_gc_memcpy_barrier(%dst, len)
// Destinations whose underlying object is an alloca (stack) or a global are
// skipped — those roots are rescanned during the STW remark. Stored values that
// are constants (null/undef/globals) are never heap pointers, so skipped.
//
//===----------------------------------------------------------------------===//

#include "llvm/Analysis/ValueTracking.h"
#include "llvm/IR/IRBuilder.h"
#include "llvm/IR/InstIterator.h"
#include "llvm/IR/Instructions.h"
#include "llvm/IR/IntrinsicInst.h"
#include "llvm/IR/Module.h"
#include "llvm/IR/PassManager.h"
#include "llvm/Passes/PassBuilder.h"
#include "llvm/Plugins/PassPlugin.h"

using namespace llvm;

namespace {

bool isStackOrGlobalDest(Value *Dst) {
  const Value *Base = getUnderlyingObject(Dst);
  return isa<AllocaInst>(Base) || isa<GlobalValue>(Base);
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
      for (Instruction &I : instructions(F)) {
        if (auto *SI = dyn_cast<StoreInst>(&I)) {
          if (SI->getValueOperand()->getType()->isPtrOrPtrVectorTy())
            Stores.push_back(SI);
        } else if (auto *MT = dyn_cast<AnyMemTransferInst>(&I)) {
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
    }

    if (NStore || NVec || NMem || NSkipStack)
      errs() << "solar-write-barriers: " << NStore << " store, " << NVec
             << " vector, " << NMem << " memcpy inserted; " << NSkipStack
             << " stack/global skipped\n";

    return (NStore || NVec || NMem) ? PreservedAnalyses::none()
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
                  return false;
                });
          }};
}
