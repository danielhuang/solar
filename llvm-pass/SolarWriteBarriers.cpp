// LLVM passes for Solar GC allocation lowering and write barriers.
//
// `solar-specialize-gc-alloc` selects fixed-class allocators for constant
// request sizes and exposes pointer-free copies to LLVM. `solar-write-barriers`
// instruments heap pointer writes after optimization.

#include "llvm/Analysis/ValueTracking.h"
#include "llvm/Analysis/CFG.h"
#include "llvm/ADT/SmallPtrSet.h"
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

// Run after the last LLVM optimization pipeline. A function-entry poll covers
// recursion and a poll on every DFS backedge covers all loops (including
// irreducible control flow). Runtime and generated _mark_* functions must not
// poll: their callers may hold collector locks or be GC workers themselves.
struct SolarSafepoints : PassInfoMixin<SolarSafepoints> {
  PreservedAnalyses run(Module &M, ModuleAnalysisManager &) {
    LLVMContext &Ctx = M.getContext();
    auto *PageTy = ArrayType::get(Type::getInt8Ty(Ctx), 4096);
    auto *Page = M.getNamedGlobal("SOL_SAFEPOINT_PAGE");
    if (!Page) {
      Page = new GlobalVariable(M, PageTy, false, GlobalValue::ExternalLinkage,
                                nullptr, "SOL_SAFEPOINT_PAGE");
      Page->setAlignment(Align(4096));
      Page->setVisibility(GlobalValue::HiddenVisibility);
      Page->setDSOLocal(true);
    }
    for (Function &F : M) {
      if (F.isDeclaration() || !F.getName().starts_with("solar_"))
        continue;
      // These attributes were inferred before polls existed. A poll can enter
      // the runtime and synchronize with the collector through a signal trap.
      F.removeFnAttr(Attribute::Memory);
      F.removeFnAttr(Attribute::NoSync);
      F.removeFnAttr(Attribute::Speculatable);
      SmallPtrSet<Instruction *, 16> Points;
      Points.insert(&*F.getEntryBlock().getFirstInsertionPt());
      SmallVector<std::pair<const BasicBlock *, const BasicBlock *>, 16> Edges;
      FindFunctionBackedges(F, Edges);
      for (auto [From, To] : Edges)
        Points.insert(const_cast<BasicBlock *>(From)->getTerminator());
      for (Instruction *At : Points) {
        IRBuilder<> B(At);
        auto *Poll = B.CreateLoad(Type::getInt8Ty(Ctx), Page, true);
        Poll->setAlignment(Align(1));
        Poll->setDebugLoc(barrierDebugLoc(At));
      }
    }
    for (Function &F : M)
      for (Instruction &I : instructions(F))
        if (auto *Call = dyn_cast<CallBase>(&I))
          if (Function *Callee = Call->getCalledFunction())
            if (Callee->getName().starts_with("solar_")) {
              Call->removeFnAttr(Attribute::Memory);
              Call->removeFnAttr(Attribute::NoSync);
              Call->removeFnAttr(Attribute::Speculatable);
            }
    return PreservedAnalyses::none();
  }
  static bool isRequired() { return true; }
};

bool isStackOrGlobalDest(Value *Dst) {
  const Value *Base = getUnderlyingObject(Dst);
  return isa<AllocaInst>(Base) || isa<GlobalValue>(Base);
}

// LLVM's x86 backend emits an unresolved `__llvm_memcpy_element_unordered_atomic_16`
// for this intrinsic. Solar's atomic128 runtime already provides the required
// unordered i128 load/store semantics, so lower each element back to those
// operations after optimization (keeping optimization's escape analysis intact).
struct SolarLowerAtomicMemcpy16 : PassInfoMixin<SolarLowerAtomicMemcpy16> {
  PreservedAnalyses run(Module &M, ModuleAnalysisManager &) {
    SmallVector<CallInst *, 8> Copies;
    for (Function &F : M)
      for (Instruction &I : instructions(F))
        if (auto *Call = dyn_cast<CallInst>(&I))
          if (Function *Callee = Call->getCalledFunction())
            if (Callee->getName().starts_with(
                    "llvm.memcpy.element.unordered.atomic.")) {
              auto *ElementBytes = dyn_cast<ConstantInt>(Call->getArgOperand(3));
              if (ElementBytes && ElementBytes->getZExtValue() == 16)
                Copies.push_back(Call);
            }

    if (Copies.empty())
      return PreservedAnalyses::all();

    LLVMContext &Ctx = M.getContext();
    Type *PtrTy = PointerType::getUnqual(Ctx);
    FunctionType *CopyTy = FunctionType::get(
        Type::getVoidTy(Ctx), {PtrTy, PtrTy}, false);
    FunctionCallee Copy = M.getOrInsertFunction("sol_copy_128_unordered", CopyTy);

    for (CallInst *Call : Copies) {
      // The intrinsic requires each atomic element to be naturally aligned.
      // The helper uses align-16 atomic operations, so retain that precondition.
      if (Call->getParamAlign(0).valueOrOne() < Align(16) ||
          Call->getParamAlign(1).valueOrOne() < Align(16))
        report_fatal_error("unaligned 16-byte atomic memcpy intrinsic");
      if (Function *CopyFunction = dyn_cast<Function>(Copy.getCallee()))
        CopyFunction->addFnAttr(Attribute::NoInline);

      Value *Dst = Call->getArgOperand(0);
      Value *Src = Call->getArgOperand(1);
      Value *Length = Call->getArgOperand(2);
      auto *LengthTy = cast<IntegerType>(Length->getType());
      Function *F = Call->getFunction();
      BasicBlock *Preheader = Call->getParent();
      BasicBlock *Continue = Preheader->splitBasicBlock(
          Call->getIterator(), "atomic.memcpy.continue");
      Preheader->getTerminator()->eraseFromParent();
      BasicBlock *Loop = BasicBlock::Create(Ctx, "atomic.memcpy.loop", F, Continue);
      BasicBlock *Body = BasicBlock::Create(Ctx, "atomic.memcpy.body", F, Continue);

      IRBuilder<> Before(Preheader);
      Before.CreateBr(Loop);
      IRBuilder<> LoopBuilder(Loop);
      PHINode *Offset = LoopBuilder.CreatePHI(LengthTy, 2, "atomic.memcpy.offset");
      Offset->addIncoming(ConstantInt::get(LengthTy, 0), Preheader);
      Value *More = LoopBuilder.CreateICmpULT(Offset, Length);
      LoopBuilder.CreateCondBr(More, Body, Continue);

      IRBuilder<> BodyBuilder(Body);
      Value *ElementDst = BodyBuilder.CreateGEP(Type::getInt8Ty(Ctx), Dst, Offset);
      Value *ElementSrc = BodyBuilder.CreateGEP(Type::getInt8Ty(Ctx), Src, Offset);
      BodyBuilder.CreateCall(Copy, {ElementDst, ElementSrc});
      Value *Next = BodyBuilder.CreateAdd(Offset, ConstantInt::get(LengthTy, 16));
      BodyBuilder.CreateBr(Loop);
      Offset->addIncoming(Next, Body);

      Call->eraseFromParent();
    }
    return PreservedAnalyses::none();
  }
};

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

// Coalesce equal allocations in batches of up to eight within a block. Only pure
// instructions and initialization stores to earlier objects in the group may
// intervene: never hoist allocations across calls, publication, or control flow.
// Run after allocation elision, before barriers and safepoint insertion.
struct SolarBatchGcAlloc : PassInfoMixin<SolarBatchGcAlloc> {
  // O3 may propagate constant arguments into the internal allocator wrapper
  // and remove them from its signature. Recover the request from its tail call.
  static SmallVector<Value *, 3> request(CallInst *Call) {
    Function *F = Call ? Call->getCalledFunction() : nullptr;
    if (!F || !F->getName().starts_with("sol_alloc_class_") ||
        F->getName().contains("_batch") ||
        !Call->getType()->isPointerTy() ||
        Call->hasOperandBundles() || Call->isMustTailCall())
      return {};
    SmallVector<Value *, 3> Args;
    if (F->isDeclaration()) {
      for (Value *V : Call->args())
        Args.push_back(V);
    } else {
      for (Instruction &I : instructions(F)) {
        auto *Inner = dyn_cast<CallInst>(&I);
        Function *Target = Inner ? Inner->getCalledFunction() : nullptr;
        if (!Target || !Target->getName().starts_with("sol_alloc_class_") ||
            !Target->getName().ends_with("_impl"))
          continue;
        if (!Args.empty())
          return {};
        for (Value *V : Inner->args()) {
          if (auto *A = dyn_cast<Argument>(V))
            V = Call->getArgOperand(A->getArgNo());
          Args.push_back(V);
        }
      }
    }
    if (Args.size() != 3 || !llvm::all_of(Args, [](Value *V) {
          return isa<Constant>(V);
        }))
      return {};
    auto *Size = dyn_cast<ConstantInt>(Args[0]);
    auto *Alignment = dyn_cast<ConstantInt>(Args[1]);
    if (!Size || !Alignment || Size->getBitWidth() != 64 ||
        Alignment->getBitWidth() != 64 ||
        std::max(Size->getZExtValue(), Alignment->getZExtValue()) >
            (UINT64_C(1) << 30))
      return {};
    return Args;
  }
  PreservedAnalyses run(Module &M, ModuleAnalysisManager &) {
    LLVMContext &Ctx = M.getContext();
    Type *Ptr = PointerType::getUnqual(Ctx);
    Type *I64 = Type::getInt64Ty(Ctx);
    unsigned Batches = 0, NewbornValues = 0;
    for (Function &F : M) {
      if (F.isDeclaration() || !isGeneratedFunc(F))
        continue;
      for (BasicBlock &BB : F) {
        SmallVector<CallInst *, 16> Region;
        auto Flush = [&]() {
          while (!Region.empty()) {
            auto Args = request(Region.front());
            SmallVector<CallInst *, 8> Group;
            for (CallInst *Call : Region)
              if (Group.size() < 8 && request(Call) == Args)
                Group.push_back(Call);
            llvm::erase_if(Region, [&](CallInst *Call) {
              return llvm::is_contained(Group, Call);
            });
            unsigned Count = Group.size();
            if (Count == 1) {
              Group.front()->setMetadata("solar.gc.newborn", MDNode::get(Ctx, {}));
              ++NewbornValues;
              continue;
            }
            IRBuilder<> B(Group.front());
            B.SetCurrentDebugLocation(barrierDebugLoc(Group.front()));
            uint64_t Need = std::max<uint64_t>({
                cast<ConstantInt>(Args[0])->getZExtValue(),
                cast<ConstantInt>(Args[1])->getZExtValue(), 8});
            unsigned Class = Log2_64_Ceil(Need) - 3;
            std::string Name =
                ("sol_alloc_class_" + Twine(Class) + "_batch" + Twine(Count)).str();
            if (Count == 2) {
              // x86-64 SysV returns the two-pointer repr(C) aggregate in
              // two integer registers; larger batches use an address view.
              auto *ResultTy = StructType::get(Ctx, {I64, I64});
              auto Batch = M.getOrInsertFunction(
                  Name, FunctionType::get(ResultTy, {I64, I64, Ptr}, false));
              auto *BatchCall = B.CreateCall(Batch, Args);
              BatchCall->setDoesNotThrow();
              for (unsigned N = 0; N != Count; ++N) {
                auto *Address = cast<Instruction>(
                    B.CreateIntToPtr(B.CreateExtractValue(BatchCall, N), Ptr));
                Address->setMetadata("solar.gc.newborn", MDNode::get(Ctx, {}));
                Group[N]->replaceAllUsesWith(Address);
                ++NewbornValues;
              }
            } else {
              auto *SlotsTy = ArrayType::get(Ptr, Count);
              auto Batch = M.getOrInsertFunction(
                  Name + "_view", FunctionType::get(Ptr, {I64, I64, Ptr}, false));
              auto *BatchCall = B.CreateCall(Batch, Args);
              BatchCall->setDoesNotThrow();
              // The view aliases allocator TLS. Read all addresses immediately;
              // do not mark it noalias or invariant, since the next allocator
              // call may overwrite the same storage.
              for (unsigned N = 0; N != Count; ++N) {
                Value *Slot = B.CreateConstInBoundsGEP2_32(SlotsTy, BatchCall, 0, N);
                auto *Address = B.CreateLoad(Ptr, Slot, "alloc.address");
                // Later optimization must not sink a borrowed-view read past
                // a loop where the final safepoint pass will insert a poll.
                // A live volatile result must be materialized, not reloaded
                // from allocator storage after the collector may have run.
                Address->setVolatile(true);
                Address->setMetadata("solar.gc.newborn", MDNode::get(Ctx, {}));
                Group[N]->replaceAllUsesWith(Address);
                ++NewbornValues;
              }
            }
            for (CallInst *Old : Group)
              Old->eraseFromParent();
            ++Batches;
          }
        };
        // Delay rewriting until the region is complete so initialization
        // stores can still be recognized by their original allocation base.
        for (Instruction &I : BB) {
          auto Args = request(dyn_cast<CallInst>(&I));
          if (!Args.empty()) {
            Region.push_back(cast<CallInst>(&I));
            continue;
          }
          if (auto *Store = dyn_cast<StoreInst>(&I)) {
            const Value *Dest = getUnderlyingObject(Store->getPointerOperand());
            if (!Store->isVolatile() && !Store->isAtomic() &&
                llvm::is_contained(Region, Dest))
              continue;
          }
          if (I.mayReadOrWriteMemory() || I.mayHaveSideEffects() ||
              !isSafeToSpeculativelyExecute(&I))
            Flush();
        }
        Flush();
      }
    }
    if (Batches)
      errs() << "solar-batch-gc-alloc: " << Batches << " batches\n";
    return (Batches || NewbornValues) ? PreservedAnalyses::none()
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
        "sol_write_barrier", FunctionType::get(VoidTy, {PtrTy}, false));
    FunctionCallee MemB = M.getOrInsertFunction(
        "sol_gc_memcpy_barrier",
        FunctionType::get(VoidTy, {PtrTy, I64}, false));

    unsigned NStore = 0, NVec = 0, NMem = 0, NSkipStack = 0, NSkipPlain = 0;
    unsigned NSkipNewborn = 0;

    for (Function &F : M) {
      if (F.isDeclaration())
        continue;
      StringRef Name = F.getName();
      if (!(Name.starts_with("solar_") || Name == "main"))
        continue;

      // Collect first because instrumentation mutates the instruction list.
      SmallVector<StoreInst *, 32> Stores;
      SmallVector<AnyMemTransferInst *, 8> Mems;
      SmallVector<AtomicRMWInst *, 8> Exchanges;
      SmallVector<AtomicCmpXchgInst *, 8> Compares;
      SmallVector<CallInst *, 8> WideCalls;
      SmallPtrSet<StoreInst *, 32> NewbornStores;
      // Allocations are born black during concurrent marking. An exact new
      // allocation value needs no shading until a possible suspension point.
      // Be conservative across calls, possible polling loads, and block
      // boundaries. Tagged allocation-view loads cannot be safepoints.
      // This is about the stored value: a new destination may still receive an
      // old white reference, which must retain its barrier. Pointer arithmetic
      // is deliberately not followed, since it may reach a different slot.
      for (BasicBlock &BB : F) {
        SmallPtrSet<Value *, 32> Newborn;
        for (Instruction &I : BB) {
          auto *Load = dyn_cast<LoadInst>(&I);
          if (isa<CallBase>(I) ||
              (Load && Load->isVolatile() && !I.getMetadata("solar.gc.newborn")))
            Newborn.clear();
          if (I.getType()->isPointerTy() && I.getMetadata("solar.gc.newborn")) {
            Newborn.insert(&I);
            // This proof belongs to the allocation-to-barrier pipeline stage,
            // not to arbitrary instructions produced by later optimization.
            I.setMetadata("solar.gc.newborn", nullptr);
          }
          if (auto *Store = dyn_cast<StoreInst>(&I))
            if (Newborn.contains(Store->getValueOperand()))
              NewbornStores.insert(Store);
        }
      }
      for (Instruction &I : instructions(F)) {
        if (auto *SI = dyn_cast<StoreInst>(&I)) {
          Type *VTy = SI->getValueOperand()->getType();
          // Pointer stores are precise; wider stores cover optimizer-created
          // aggregates that may contain pointer words.
          if (VTy->isPtrOrPtrVectorTy() || DL.getTypeStoreSize(VTy) > 8 ||
              (SI->isAtomic() && DL.getTypeStoreSize(VTy) == 8))
            Stores.push_back(SI);
        } else if (auto *RMW = dyn_cast<AtomicRMWInst>(&I)) {
          if (RMW->getOperation() == AtomicRMWInst::Xchg &&
              DL.getTypeStoreSize(RMW->getValOperand()->getType()) >= 8)
            Exchanges.push_back(RMW);
        } else if (auto *CX = dyn_cast<AtomicCmpXchgInst>(&I)) {
          if (DL.getTypeStoreSize(CX->getNewValOperand()->getType()) >= 8)
            Compares.push_back(CX);
        } else if (auto *Call = dyn_cast<CallInst>(&I)) {
          if (Function *Callee = Call->getCalledFunction()) {
            StringRef N = Callee->getName();
            if (N == "sol_store_128_unordered" || N == "sol_load_128_unordered" ||
                N == "sol_copy_128_unordered" || N == "sol_atomic_store_128_rel" ||
                N == "sol_atomic_load_128_acq" ||
                N == "sol_atomic_compare_exchange_128_acq_rel")
              WideCalls.push_back(Call);
          }
          // Intrinsics are also CallInsts, so collect transfers here too.
          if (auto *MT = dyn_cast<AnyMemTransferInst>(Call)) {
            if (MT->getMetadata("solar.nobarrier"))
              ++NSkipPlain;
            else
              Mems.push_back(MT);
          }
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
        if (NewbornStores.contains(SI)) {
          ++NSkipNewborn;
          continue;
        }
        IRBuilder<> B(SI->getNextNode());
        if (Val->getType()->isPointerTy() || Val->getType()->isIntegerTy(64)) {
          // C atomics represent references as integer-valued stores.
          Value *Pointer = Val->getType()->isPointerTy() ? Val : B.CreateIntToPtr(Val, PtrTy);
          CallInst *C = B.CreateCall(WB, {Pointer});
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

      auto ShadeAtomicValue = [&](Instruction *At, Value *Dst, Value *Val,
                                  Value *Succeeded) {
        if (isStackOrGlobalDest(Dst))
          return;
        IRBuilder<> B(At->getNextNode());
        if (Succeeded)
          Val = B.CreateSelect(Succeeded, Val, Constant::getNullValue(Val->getType()));
        if (Val->getType()->isPointerTy()) {
          B.CreateCall(WB, {Val})->setDebugLoc(barrierDebugLoc(At));
        } else {
          unsigned Bits = DL.getTypeStoreSize(Val->getType()) * 8;
          Type *WordTy = IntegerType::get(Ctx, Bits);
          Value *Words = B.CreateBitCast(Val, WordTy);
          for (unsigned Bit = 0; Bit < Bits; Bit += 64) {
            Value *Word = Bit ? B.CreateLShr(Words, Bit) : Words;
            Word = B.CreateZExtOrTrunc(Word, I64);
            B.CreateCall(WB, {B.CreateIntToPtr(Word, PtrTy)})
                ->setDebugLoc(barrierDebugLoc(At));
          }
        }
        ++NStore;
      };
      for (AtomicRMWInst *RMW : Exchanges)
        ShadeAtomicValue(RMW, RMW->getPointerOperand(), RMW->getValOperand(), nullptr);
      for (AtomicCmpXchgInst *CX : Compares) {
        if (isStackOrGlobalDest(CX->getPointerOperand()))
          continue;
        IRBuilder<> B(CX->getNextNode());
        Value *Succeeded = B.CreateExtractValue(CX, 1);
        // Insert after the success extraction so all uses are dominated.
        ShadeAtomicValue(cast<Instruction>(Succeeded), CX->getPointerOperand(),
                         CX->getNewValOperand(), Succeeded);
      }
      for (CallInst *Call : WideCalls) {
        IRBuilder<> B(Call->getNextNode());
        auto ShadeDestination = [&](unsigned Arg) {
          Value *Dst = Call->getArgOperand(Arg);
          if (!isStackOrGlobalDest(Dst)) {
            B.CreateCall(MemB, {Dst, ConstantInt::get(I64, 16)})
                ->setDebugLoc(barrierDebugLoc(Call));
            ++NMem;
          }
        };
        ShadeDestination(0);
        if (Call->getCalledFunction()->getName() ==
            "sol_atomic_compare_exchange_128_acq_rel")
          ShadeDestination(1);
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
    if (NSkipNewborn)
      errs() << "solar-write-barriers: " << NSkipNewborn
             << " newborn-value barriers omitted\n";

    return (NStore || NVec || NMem || NSkipNewborn) ? PreservedAnalyses::none()
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
                  if (Name == "solar-batch-gc-alloc") {
                    MPM.addPass(SolarBatchGcAlloc());
                    return true;
                  }
                  if (Name == "solar-write-barriers") {
                    MPM.addPass(SolarWriteBarriers());
                    return true;
                  }
                  if (Name == "solar-lower-atomic-memcpy16") {
                    MPM.addPass(SolarLowerAtomicMemcpy16());
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
                  if (Name == "solar-safepoints") {
                    MPM.addPass(SolarSafepoints());
                    return true;
                  }
                  return false;
                });
          }};
}
