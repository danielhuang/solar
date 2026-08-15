---
name: commit-messages
description: Create, draft, or amend Git commits with concise subjects and decision-rich bodies that summarize the relevant conversation, including attempted approaches, decisions, reasoning, tradeoffs, measurements, and validation. Use when the user asks Codex to commit changes, write a commit message, improve a commit body, or preserve the development discussion in Git history.
---

# Commit Messages

Create useful project history: describe both what changed and why the work took
its final shape. Keep every statement grounded in the diff, command output, or
relevant conversation.

## Workflow

1. Determine whether the user requested an actual commit, a draft message, or an
   amendment. Do not mutate Git state when the request is only to draft or review.
2. Inspect `git status`, the relevant diff, and recent commits before composing.
   Preserve unrelated work and stage only files within the requested scope.
3. Write an imperative subject that identifies the outcome. Prefer 72 characters
   or fewer and omit a trailing period.
4. Write a body that summarizes the task's relevant conversation, not merely the
   final diff. Include whichever of these actually occurred:

   - the requested outcome and implementation;
   - approaches attempted or investigated, and why they were rejected;
   - important decisions, constraints, reasoning, and tradeoffs;
   - measurements or before/after results, with enough methodology to interpret them;
   - validation performed, failures encountered, and known environmental limits.

5. Use short prose paragraphs ordered from outcome to rationale, alternatives,
   results, and validation. Avoid diary-like chronology, raw logs, vague claims,
   or details unrelated to the committed changes.
6. Never invent attempts, measurements, or test results. Distinguish a product
   failure from an environmental failure and report incomplete validation plainly.
7. Create the commit only after confirming the staged diff matches the intended
   scope. Do not amend, force, sign, push, or add co-authors unless requested.
8. Verify the resulting commit and report its hash, subject, and remaining
   worktree state.

## Message Shape

Use this adaptable structure rather than mandatory headings:

```text
Imperative summary of the outcome

Explain the implemented change and the user-visible or architectural result.

Record the alternatives investigated, why they failed or were discarded, and
the reasoning behind the selected design. Include meaningful tradeoffs.

Report measurements and validation accurately. Mention noisy data, skipped
checks, or environment-caused failures when relevant.
```

For a small, straightforward change, keep the body brief while still recording
the relevant decision. For a complex investigation, preserve the important
reasoning without copying the entire conversation.
