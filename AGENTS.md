# Working agreements

- Use Rust for application code, examples, and tests. When Python is appropriate for auxiliary scripts, use `uv`; never install packages into the system Python.
- Always be direct and concise.
- Do not be lazy!

## Branching Strategy

- Do not make changes directly on `main`.
- Top level Features will branch off of `main` and use the format `feat/PROJECT_NAME` for the project specifications.
- When working on a step/stage of a project, we branch off of the feat branch and create a branch in the format `stage/STAGE_NAME`.
- Always PR the Stage branch to the Feature/Project Branch and project branch PRs into `main`.

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
