# Working agreements

- Use Rust for application code, examples, and tests. When Python is appropriate for auxiliary scripts, use `uv`; never install packages into the system Python.
- Always be direct and concise.
- Do not be lazy!

## Branching and versioning

Do not make changes directly on `main`. Branch from `main` and open a PR back
to `main` for each of these workflows.

| Workflow | Branch | Package version |
| --- | --- | --- |
| Bug or defect fix | `bug/NAME` | Increment patch by one. |
| Feature | `feat/PROJECT_NAME` | Increment minor or major by one. Record the human's choice when drafting the feature overview, specification, or outline. |
| Simple enhancement | `enhance/NAME` | For a small improvement, confirm a patch or minor increment with the human before work begins. |
| Ad hoc maintenance | `adhoc/NAME` | No bump for changes limited to AI rules, documentation (including the README), or CI, with no application code changes. |

For a staged feature, create `stage/STAGE_NAME` branches from the feature branch
and open their PRs against that feature branch. All stages share the feature's
target release version.

- Use the package version in `Cargo.toml` on `main` as the baseline. A major
  bump resets minor and patch to zero. A minor bump resets patch to zero.
  Recheck `main` before finalizing and confirm any revised target with the human.
- For a versioned change, development iterations may use prereleases of the
  target version, such as `0.2.0-dev.1` and `0.2.0-dev.2`. Keep the package
  version in `Cargo.toml` and its entry in `Cargo.lock` in sync.
- Finalize the version on the work branch after final review and explicit human
  approval, before merging the PR into `main`. Remove any development suffix
  and verify the manifest and lockfile agree. If the human handles finalization,
  check for their version change and clarify the target or ownership if needed.

## Specification Documents

- Specifications are iterative working documents. Update them as decisions change;
  distinguish requirements, proposals, open questions, and approved decisions.
- Start each project at `specs/NNN_project_name/overview.md`. Capture the complete
  functional scope, constraints, and unresolved decisions at a high level first.
- Obtain explicit user approval of the overview before breaking down the entire project into self-contained stages.
- Then describe the observable outcome expected at the end of each stage in the overview, and elaborate stage details in `NN_stage_name.md` files.
- Keep stage specifications concise, clear, and self-contained. Specify behavior,
  interfaces, failure cases, and acceptance criteria in enough detail for a human
  to implement the stage without relying on chat history.
- End each stage specification with a `Validation record` section. Keep actual
  check results, acceptance evidence, tested environments, limitations, and agreed
  deferrals there; do not create a separate validation Markdown file. While a
  stage is only a design draft, mark validation as pending. Link to this section
  from the overview and other documentation, and update it as checks are run.

- Approval of a specification does not itself authorize implementation. Follow the
  user's current scope; do not create application scaffolding, implementation,
  stage files, or tests before the corresponding work is authorized.
- A workflow where the agent writes functional/system tests and the user writes
  implementation is an option to discuss later, not an established requirement.

## Test-first development and review

1. Define the observable behavior and the constraints that must hold on success
   and failure. For a small change, the test can be the behavior record. For
   stage work, link the affected acceptance criteria to checks in the existing
   `Validation record`.
2. For a behavior change, write or extend the focused test before changing the
   implementation. Run it and confirm it fails for the expected reason. An
   existing test that reproduces the problem serves the same purpose. When
   automation is impractical, define a repeatable manual check before editing
   and record what you can verify before and after the change.
3. Make the smallest change that satisfies the behavior and its constraints.
   Run the focused check and relevant regressions through the normal entry
   points. Report what passed and any behavior or environment still unverified.

- When a change affects overlapping operations, include a test that controls
  the important ordering with explicit synchronization. Use Loom when a small
  synchronization component needs several thread orderings explored. Use
  integration tests for real file, process, or terminal behavior.
- When a change alters a guard that protects user data, check that the test
  catches a deliberate break in that guard after the baseline passes. Use an
  isolated copy and either a focused `cargo-mutants` run or a single deliberate
  mutation. Confirm the intended assertion fails. A build failure alone gives
  no evidence about that assertion.
- Any agent assigned a review must read and follow [REVIEW.md](REVIEW.md) before
  starting. For substantial or risky changes, or when the user requests a
  review, delegate to a separate Review agent. In the handoff, explicitly tell
  the reviewer to read and follow `REVIEW.md` and provide the requirements, base
  and diff, and relevant tests. Use a fresh context when available. Address
  findings, rerun affected checks, and have the reviewer inspect material fixes.
  If a separate reviewer is unavailable, follow `REVIEW.md` for the self-review
  and identify it as such.

## Programming Style and Guidelines

### Rust

- Design structured, modular, idiomatic Rust with clear ownership and small,
  cohesive interfaces. Use *Rust for Rustaceans* as a design guide. Favor readable
  code over convoluted abstractions or unnecessary verbosity.
- Always format code with `cargo fmt`.
- Run `cargo clippy` and fix all warnings. Allow only narrowly scoped exceptions
  accompanied by a written reason.
- Always ensure test cases are successful before pushing and being done. If something fails, fix it. Do NOT be LAZY!
  If checks cannot run, report the blocker and do not claim verification succeeded.
- Be cognizant of external dependencies. Always ensure you are not pulling in old/decrepit dependencies. Use them when absolutely needed.
- Write module, type and function level documentation comments in such a manner a new person to the project understands everything about the entity and can use and extend it.

#### Testing

- Put unit tests in inline `#[cfg(test)]` modules in the same Rust source file
  as the code they test.
- Put integration tests in the top-level `tests/` directory.
- Use submodules wherever they help organize implementation code.

### Python

- Never install anything in the system Python.
- Always use `uv` to run scripts and manage python versions and virtual environments.
- When scripts are going to be persisted, lint and format them with `ruff` and type check with `ty`.
- Always write documentation and docstrings.
- Add type hints everywhere possible.


### Bash/Shell

- Attempt to write any scripts in POSIX compliant shell.
- Run shellcheck to lint scripts if they are going to be persisted in the repo.
- Always follow industry standard practices.
