---
name: rust-guidelines
description: Microsoft's Pragmatic Rust Guidelines, for writing and reviewing Rust in this project. Use when writing or changing Rust code, reviewing a diff or a crate, or answering questions about Rust design, performance, memory use, panics, unsafe code, error handling, logging, documentation or project layout. Each rule has an ID (M-...) to cite in findings.
---

# Pragmatic Rust Guidelines (Microsoft)

Microsoft's guidelines for idiomatic Rust "that scales", written for agents
as well as people. Source: <https://microsoft.github.io/rust-guidelines/>
(agent file `agents/all.txt`), MIT licence, Copyright (c) Microsoft
Corporation; see `LICENSE.md`. The chapter files below are that file, split
by chapter and otherwise unchanged.

## How to use

1. Pick the chapters that fit the task from the index below, and read only
   those files. Read a whole chapter before applying its rules: each rule
   explains its reasons, exceptions and examples.
2. When reviewing, cite each finding by its rule ID (for example
   `M-MEM-REUSE`), with the file and line, what the code does, and what the
   rule asks instead.
3. Framecut is an application (a binary crate with library crates beside
   it), so the Application chapter applies to it. The Library chapters apply
   to its library crates (`framecut-capture`, `framecut-platform`) where
   they make sense for an application's internals.
4. The guidelines are advice, not law. Where one conflicts with this
   project's conventions or measurements, say so rather than following it
   blindly.

## Index

### Universal Guidelines: `guidelines/universal.md`

- M-DOCUMENTED-MAGIC: Magic values are documented
- M-LINT-OVERRIDE-EXPECT: Lint overrides should use `#[expect]`
- M-LOG-STRUCTURED: Use structured logging with message templates
- M-PUBLIC-DEBUG: Public types are Debug
- M-PUBLIC-DISPLAY: Public types meant to be read are Display
- M-REGULAR-FN: Prefer regular over associated functions
- M-SHORT-NAMES: Names of items are short
- M-SMALLER-CRATES: If in doubt, split the crate
- M-STATIC-VERIFICATION: Use static verification
- M-UPSTREAM-GUIDELINES: Follow the upstream guidelines
- M-WEASEL-WORDS: Names are free of weasel words

### Correctness Guidelines: `guidelines/correctness.md`

- M-PANIC-CONTINUATION: Panic continuation is last resort
- M-PANIC-IS-STOP: Panic means 'stop the program'
- M-PANIC-MESSAGE: Custom panics have a helpful message
- M-PANIC-ON-BUG: Detected programming bugs are panics, not errors
- M-UNSAFE-IMPLIES-UB: Unsafe implies undefined behavior
- M-UNSAFE: Unsafe needs reason, should be avoided
- M-UNSOUND: All code must be sound

### Performance Guidelines: `guidelines/performance.md`

- M-ASYNC-STACK-SIZE: Hot `async` functions reduce stack size
- M-AVOID-INDIRECTION: Nested type hierarchies should avoid needless indirection
- M-BOX-DST: Use boxed slices and strings for immutable owned sequences
- M-FAST-HASHER: Use a fast hasher where possible
- M-HOTPATH: Identify, profile, optimize the hot path early
- M-INITIAL-CAPACITY: Collections are created with sufficient initial capacity
- M-LOG-OVERHEAD: Library telemetry does not tank performance
- M-MEM-REUSE: Reuse allocations where possible
- M-SHRINK-TO-FIT: Shrink collections to fit after building
- M-THROUGHPUT: Optimize for throughput, avoid empty cycles
- M-YIELD-POINTS: Long-running tasks should have yield points

### Application Guidelines: `guidelines/applications.md`

- M-APP-ERROR: Applications may use Anyhow or derivatives
- M-MIMALLOC-APPS: Use mimalloc for apps
- M-TARGET-CPU: Applications target highest viable target-cpu

### Documentation: `guidelines/documentation.md`

- M-CANONICAL-DOCS: Documentation has canonical sections
- M-DOC-INLINE: Mark `pub use` items with `#[doc(inline)]`
- M-FIRST-DOC-SENTENCE: First sentence is one line; approx. 15 words
- M-MODULE-DOCS: Has comprehensive module documentation

### Project Guidelines: `guidelines/project.md`

- M-CARGO-WORKSPACE: Common settings come from the workspace Cargo.toml
- M-CRATES-FLAT-FOLDER: All crates are siblings in one folder
- M-CRATES-IN-WORKSPACE: The workspace lists and versions all crates
- M-LATEST-EDITION: New crates target latest edition
- M-MSRV: MSRV is conservatively updated

### AI Guidelines: `guidelines/ai.md`

- M-DESIGN-FOR-AI: Design with AI use in mind
- M-NO-META-DESIGN-DOCUMENTATION: Avoid meta design documentation
- M-RUST-SHAPED: Rust code solves Rust problems
- M-SINGLE-ITEM-PATH: Items are only visible through one path
- M-TAUTOLOGICAL-TESTS: Tests do not assert ground truth

### Libraries / Resilience Guidelines: `guidelines/libraries-resilience.md`

- M-AVOID-STATICS: Avoid statics
- M-BUILD-RESULT: Builders validate in final `.build()`
- M-INTEGRATION-TESTS: Integration tests live under `tests/`
- M-LOG-NOT-PRINT: Production code uses telemetry, not println
- M-MOCKABLE-SYSCALLS: I/O and system calls are mockable
- M-NO-GLOB-REEXPORTS: Don't glob re-export items
- M-STRONG-TYPES-GUARD: Newtypes guard their invariants
- M-STRONG-TYPES: Use the proper type family
- M-TEST-UTIL: Test utilities are feature gated

### Libraries / UX Guidelines: `guidelines/libraries-ux.md`

- M-ASYNC-FN: Functions are `async` over returning a Future
- M-AVOID-WRAPPERS: Avoid smart pointers and wrappers in APIs
- M-BALANCED-MODULES: Modules are balanced in size and scope
- M-COLLECTION-TRAITS: Collections implement the appropriate iter traits
- M-DI-HIERARCHY: Prefer types over generics, generics over dyn traits
- M-ERRORS-CANONICAL-STRUCTS: Errors are canonical structs
- M-ESSENTIAL-FN-INHERENT: Essential functionality should be inherent
- M-FROM-ERROR: Canonical error conversion uses `From`, not `map_err`
- M-INIT-BUILDER: Complex type construction has builders
- M-INIT-CASCADED: Complex type initialization hierarchies are cascaded
- M-NO-PRELUDE: Don't define preludes
- M-PARAMETER-CONSISTENCY: Parameter ordering is consistent
- M-SERVICES-CLONE: Services are Clone
- M-SIMPLE-ABSTRACTIONS: Abstractions don't visibly nest

### Libraries / Interoperability Guidelines: `guidelines/libraries-interop.md`

- M-DONT-LEAK-TYPES: Don't leak external types
- M-ESCAPE-HATCHES: Native escape hatches
- M-FOREIGN-REEXPORTS: Items come from their original crate
- M-IMPL-ASREF: Accept `impl AsRef<>` where feasible
- M-IMPL-IO: Accept `impl 'IO'` where feasible ('sans IO')
- M-IMPL-RANGEBOUNDS: Accept `impl RangeBounds<>` where feasible
- M-TYPES-SEND: Types are Send

### Libraries / Building Guidelines: `guidelines/libraries-building.md`

- M-FEATURES-ADDITIVE: Features are additive
- M-OOBE: Libraries work out of the box
- M-SYS-CRATES: Native `-sys` crates compile without dependencies

### FFI Guidelines: `guidelines/ffi.md`

- M-FFI-NAMING: FFI crates follow established naming conventions
- M-FFI-TRANSLATES: Business logic belongs in core crates, FFI only translates
- M-ISOLATE-DLL-STATE: Isolate DLL state between FFI libraries

### Macros Guidelines: `guidelines/macros.md`

- M-EXAMPLE-OVER-PROC: Prefer 'macros by example' over proc macros
- M-MACRO-HELPERS: Third party items come from hidden `_private` module
- M-MACRO-LAST-RESORT: Macros are a last resort
- M-MACRO-MAIN-CRATE: Macros assume main crate
- M-MACRO-VERSION-PIN: Pin supporting proc macro crates
- M-MACROS-DONT-LIE: Macros don't lie about signatures
- M-PROC-IMPL: Proc macros should have separate impl crate incl. tests
- M-PROC-IMPLIED-ITEMS: Proc macros don't produce implied or hidden items

