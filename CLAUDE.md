# Project

Coulisse is a single Rust binary that reads a `coulisse.yaml` file and spins up an OpenAI-compatible HTTP server. You point your existing tools, SDKs, and projects at it like any other OpenAI endpoint — and everything configurable lives in that one YAML file.

The goal is to collapse the plumbing that every multi-agent project ends up re-implementing: memory, workflows, multi-agent orchestration, multi-backend routing, rate limiting, tools. You describe the setup in YAML and pilot the whole thing from there, instead of writing glue code for each prototype.

# Architecture

One crate per YAML section. Coulisse's features map 1:1 to the top-level sections of `coulisse.yaml`: `agents`, `backends` (`providers:`), `experiments`, `judges`, `limits`, `memory`, `mcp`, `studio`, `telemetry`. Each is its own crate.

**Dependency rule.** Feature crates depend only on `coulisse-core` (a tiny crate of shared domain types and traits). Feature crates never depend on each other. The `cli` crate is the only place that depends on every feature crate; it is the orchestrator.

The single defensible exception: `agents → backends`, because backends is an interface to the outside world (LLM APIs), not a feature in the same sense. If that line ever feels wrong, replace it with a `Completer` trait in `coulisse-core`.

**Each feature crate owns:**

- its `Config` struct (parses its own YAML slice)
- its tables and queries — `memory` owns `messages`/`memories`, `judge` owns `scores`, `telemetry` owns `events`/`tool_calls`, `limits` owns `rate_limit_windows`. Cli opens one shared SQLite pool (via `memory::open_pool`) and hands clones to each crate; each crate runs its own `CREATE TABLE IF NOT EXISTS`. No crate exposes its pool to siblings, and no crate is the de-facto database layer.
- its public methods, named for what they do (`Limits::check`, `Memory::assemble_context`, `Agents::complete`, `Judges::append_score`)
- its admin HTTP router via `admin_router(&self) -> Option<Router>`, mounted by cli at `/admin/<name>`
- its background tasks (the feature spawns its own `tokio::spawn`; cli does not manage task lifecycles)

`memory` is **AI memory**, not "the persistence layer." It only owns conversation history and long-term facts. Anything else (scores, observability rows, rate-limit windows) belongs to the feature that produces it. If a sibling crate needs to read what another crate stores, expose a narrow trait in `coulisse-core` (e.g. `ScoreLookup`) and let the read flow through that — never through a shared `pool()` accessor.

**`cli` is the request-flow spec.** The chat handler in `cli` reads top-to-bottom as the documentation of what happens to a request. No middleware framework, no event bus, no `Feature` trait, no `TurnContext` god struct. If you want to know the order of operations, read the handler.

**No central `config` crate.** Each crate parses its own slice of YAML. Cross-feature validation, if needed, is a small post-load pass in `cli`.

**Adding a feature** = new crate, new YAML section, lines added to cli (load config, add to `AppState`, call from handler, optional `nest()` for admin). **Disabling a feature** = remove those lines.

**Resist `coulisse-core` growth.** It holds domain primitives (`UserId`, `TurnId`, `MessageId`, `Message`, `Role`, `ToolCallKind`, `AgentScoreSummary`) and tiny cross-cutting traits (`OneShotPrompt`, `ScoreLookup`). If it grows past ~250 lines, we've put too much in. Types referenced by core traits live in core (otherwise the trait can't be implemented without taking a feature dep); everything else belongs in the feature that owns it.

**No shared `proxy` crate as orchestrator.** `proxy` is a leaf crate: OpenAI wire schema, SSE helpers, error mapping. The handler lives in `cli`.

# Documentation

The user-facing mdbook lives in `docs/` (config in `docs/book.toml`, chapters in `docs/src/`). Every change that alters user-visible behavior must update the book in the same change — do not leave the docs lagging behind the code.

This applies to: new or changed YAML fields, new or removed providers, new HTTP endpoints or request/response fields, changed defaults, new features, and features moving from the roadmap to implemented (or vice versa). A user reading the book should never discover a feature that isn't there or miss one that is.

Pure internal refactors (renames, module restructuring, non-observable changes) don't need doc updates. When in doubt: if an end user could notice the change from the YAML or HTTP API, update the book.

Preview the book locally with `mdbook serve docs --port 4421`. Port 3000 is avoided because it collides with too many other dev servers; 4421 pairs with the main Coulisse port (8421) and is unlikely to clash.

# Pre-commit hook

The repo ships a pre-commit hook at `.githooks/pre-commit` that runs `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test`. A commit fails if any of them does.

Enable it in each clone with:

```
git config core.hooksPath .githooks
```

Never bypass the hook with `--no-verify`. If it fails, fix the underlying issue — the hook is the project's floor for what lands in `main`.

# Commit messages

Do not add `Co-Authored-By` trailers — no Claude co-author line, no agent attribution, no tool signature. Commit messages are the subject and body only. The git history should read as the human author's work.

# Code Principles

Apply these principles when writing code.

## Crate boundaries

Before adding code to an existing crate, ask: does this belong in a different feature crate, or a new one? `utils` modules and "shared logic" crates are the path back to coupling. If two crates need the same logic, look at crates.io first. The only legitimate shared crate is `coulisse-core` for primitives and cross-cutting traits — and it stays small.

## Sorting

Sort alphabetically by default: struct fields, object properties, function parameters, class methods, import statements, enum values, table columns. Every list.

Break this rule only when documented. Visibility grouping (constructor → public → private), natural call-convention order for function parameters, and semantically inseparable method pairs are legitimate exceptions — but the exception must exist in the code as a comment, not in your head.

## Primitives

Wrap every primitive that carries domain meaning in a dedicated type. `UserId` is not a `string`. `Email` is not a `string`. `Price` is not a `number`.

Validate once at system boundaries (user input, external API responses). Pass the strong type everywhere inside the system. Never pass a raw primitive when a domain type exists.

## Structs and Method Ownership

Data and the operations that belong with it live together. `user.save(store)`, not `UserRepository.save(user)`. `url.parse()`, not `UrlParser.parse(url)`.

Name things after what they actually are. A todo API has a `RestApi`, a `Todo`, a `Store`. It does not have a `TodoController`, `TodoService`, or `RequestHandler`. If you wouldn't say "I built a UserRepository" to a colleague, don't build one.

Free functions are almost always a smell. Every function in a `utils` file is a method on a type that doesn't exist yet. `parse_url(s)` belongs on `Url`. `format_user(user)` belongs on `User`. The 1% exception is genuinely stateless math (`clamp`, `min`, `max`) with no natural subject.

Fluent API as a design check: if `User::create(email).save(store)` reads naturally, the design is probably right. If you can't figure out what method goes on something, the design is telling you something is wrong.

A struct with 25 methods is three types that haven't been separated yet. Ask: do all these methods operate on the same data? Split what can stand alone.

## Comments

Delete comments that explain what the code does. Fix the code instead. The comment is a confession that the code failed to explain itself.

Two legitimate comment types:
1. **Context the code cannot express**: a ticket link, a bug reference, the WHY behind a workaround that looks wrong but isn't.
2. **Substantive TODOs**: what needs to change, why it wasn't done now, with a ticket reference.

Never commit commented-out code. `git log` exists.

## Migrations

Commit two files per schema change: `schema.sql` (what the database looks like now) and `migrate.sql` (how to get there from the last version). Git holds the rest.

Never accumulate numbered migration files (`V001`, `V002`, `V043`). Nobody can read the schema without replaying all of them. `schema.sql` is always the current truth. `migrate.sql` is the single step forward. The previous schema version already lives in git — you don't need it in the directory.

## Errors

Every function that can fail returns `Result<T, E>`. No panics in business logic.

```rust
// Bad — panics hide the failure mode
fn get_user(id: &str) -> User { todo!() }

// Good — honest signature
#[derive(Debug)]
enum UserError { NotFound { user_id: String }, NetworkError }

fn get_user(id: &str) -> Result<User, UserError> { ... }
```

Use the `?` operator to propagate errors without nesting. Pattern match on variants to make real decisions: retry `NetworkError`, return early on `ValidationError`.

`unwrap()` and `expect()` are only acceptable in tests or at startup for truly unrecoverable situations (missing config, no database at boot). Never in business logic.

## Dependencies

Pass dependencies explicitly through struct fields and function parameters. No global statics for business logic.

```rust
// Bad — hidden global
impl UserService {
    fn get_user(&self, id: &str) -> User {
        DATABASE.get().unwrap().query(id) // where did this come from?
    }
}

// Good — explicit
struct UserService { db: Database }

impl UserService {
    fn get_user(&self, id: &str) -> Result<User, UserError> {
        self.db.query(id)
    }
}
```

If a dependency is not in the struct or function signature, it should not exist. Wire everything in `main`. Module-level infrastructure via `OnceLock` (logger, config) is the one exception: visible at the import level, never mocked in tests.

## Testing

Use real in-memory implementations, not mocks. Mocks test your assumptions. Real implementations test your code.

```rust
// Good — enforces the actual uniqueness constraint
struct MemDatabase { users: Mutex<HashMap<String, User>> }

impl Database for MemDatabase {
    async fn insert_user(&self, user: NewUser) -> Result<User, DbError> {
        let mut users = self.users.lock().unwrap();
        if users.values().any(|u| u.email == user.email) {
            return Err(DbError::UniqueViolation("email".into()));
        }
        let created = User { id: Uuid::new_v4().to_string(), ..user.into() };
        users.insert(created.id.clone(), created.clone());
        Ok(created)
    }
}
```

Build the feature. Then test what you built. Don't write tests before you understand the shape of the code — requirements are fuzzy until they aren't. The one exception: write the failing test the moment you find a bug. At that point you have exact requirements. That's when test-first pays off. 90% unit tests (fast, in-process), 10% integration tests (one per external boundary), 0% E2E in regular CI.
