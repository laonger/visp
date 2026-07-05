//! `/init` command — generate or update `AGENTS.md`.
//!
//! Adapted from OpenCode's init command: a structured, high-signal prompt
//! that guides the LLM to create or update `AGENTS.md` for the repository.

/// Build the init prompt sent to the LLM for `/init`.
///
/// `args` — any text after `/init` (e.g. `/init focus on testing` → "focus on testing").
///          Empty when the user just types `/init`.
pub fn build_init_prompt(args: &str) -> String {
    let focus = if args.trim().is_empty() {
        "(none provided)"
    } else {
        args.trim()
    };

    format!(
        r#"Create or update `AGENTS.md` for this repository.

The goal is a compact instruction file that helps future visp sessions avoid mistakes and ramp up quickly. Every line should answer: "Would an agent likely miss this without help?" If not, leave it out.

User-provided focus or constraints (honor these):
{focus}

## How to investigate

Read the highest-value sources first:
- `README*`, root manifests, workspace config, lockfiles
- build, test, lint, formatter, typecheck, and codegen config
- CI workflows and pre-commit / task runner config
- existing instruction files (`AGENTS.md`, `CLAUDE.md`, `.cursor/rules/`, `.cursorrules`, `.github/copilot-instructions.md`)
- repo-local visp config such as `.visp/rules/`

If architecture is still unclear after reading config and docs, inspect a small number of representative code files to find the real entrypoints, package boundaries, and execution flow. Prefer reading the files that explain how the system is wired together over random leaf files. Use `codegraph_search` and `codegraph_context` to understand symbol relationships.

Prefer executable sources of truth over prose. If docs conflict with config or scripts, trust the executable source and only keep what you can verify.

## What to extract

Look for the highest-signal facts for an agent working in this repo:
- exact developer commands, especially non-obvious ones
- how to run a single test, a single package, or a focused verification step
- required command order when it matters, such as `lint -> typecheck -> test`
- monorepo or multi-package boundaries, ownership of major directories, and the real app/library entrypoints
- framework or toolchain quirks: generated code, migrations, codegen, build artifacts, special env loading, dev servers, infra deploy flow
- repo-specific style or workflow conventions that differ from defaults
- testing quirks: fixtures, integration test prerequisites, snapshot workflows, required services, flaky or expensive suites
- important constraints from existing instruction files worth preserving

Good `AGENTS.md` content is usually hard-earned context that took reading multiple files to infer.

## Questions

Only ask the user questions if the repo cannot answer something important. Use the `question` tool for one short batch at most.

Good questions:
- undocumented team conventions
- branch / PR / release expectations
- missing setup or test prerequisites that are known but not written down

Do not ask about anything the repo already makes clear.

## Writing rules

Include only high-signal, repo-specific guidance such as:
- exact commands and shortcuts the agent would otherwise guess wrong
- architecture notes that are not obvious from filenames
- conventions that differ from language or framework defaults
- setup requirements, environment quirks, and operational gotchas
- references to existing instruction sources that matter

Exclude:
- generic software advice
- long tutorials or exhaustive file trees
- obvious language conventions
- speculative claims or anything you could not verify
- content better stored in another file under `.visp/rules/`

When in doubt, omit.

Prefer short sections and bullets. If the repo is simple, keep the file simple. If the repo is large, summarize the few structural facts that actually change how an agent should work.

If `AGENTS.md` already exists, improve it in place rather than rewriting blindly. Preserve verified useful guidance, delete fluff or stale claims, and reconcile it with the current codebase.

Write the complete `AGENTS.md` using `write_file` in a single call. Do NOT rewrite the file after writing. Finish with no further tool calls after writing."#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_init_prompt_no_args() {
        let prompt = build_init_prompt("");
        assert!(prompt.contains("Create or update `AGENTS.md`"));
        assert!(prompt.contains("(none provided)"));
        assert!(prompt.contains("## How to investigate"));
        assert!(prompt.contains("## Writing rules"));
        assert!(prompt.contains("write_file"));
    }

    #[test]
    fn test_build_init_prompt_with_focus() {
        let prompt = build_init_prompt("focus on testing conventions");
        assert!(prompt.contains("focus on testing conventions"));
        assert!(!prompt.contains("(none provided)"));
        assert!(prompt.contains("## How to investigate"));
    }

    #[test]
    fn test_build_init_prompt_whitespace_only() {
        let prompt = build_init_prompt("   ");
        assert!(prompt.contains("(none provided)"));
    }
}
