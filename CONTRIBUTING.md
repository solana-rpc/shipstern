# Contributing Guide

Thank you for your interest in contributing to Shipstern! This guide will help you get started with setting up your development environment and contributing code.

## Table of Contents

1. [Getting Started](#getting-started)
2. [Setting Up Your Environment](#setting-up-your-environment)
3. [Making Changes](#making-changes)
4. [Running Tests](#running-tests)
5. [Submitting Changes](#submitting-changes)
6. [Releasing](#releasing)
7. [Code of Conduct](#code-of-conduct)

## Getting Started

To contribute to this project, you'll need Rust installed on your machine. The toolchain is pinned in [`rust-toolchain.toml`](/rust-toolchain.toml), so `rustup` installs and selects the right version on the first `cargo` command inside the repo, so you do not pick one yourself. Formatting is the exception: it runs on a separate pinned nightly, described under [Making Changes](#making-changes).

The repo is a cargo workspace. Three directories sit outside it and resolve their own lockfiles: `examples/multi-dex-stream`, `tests/idls`, and `tests/proc-macro-events` (excluded so its `program-events` feature does not unify into the rest of the workspace). `--workspace` does not reach them.

## Setting Up Your Environment

1. **Clone the Repository**

   ```sh
   git clone https://github.com/solana-rpc/shipstern.git
   cd shipstern
   ```

2. **Install Rust and Set the Toolchain**

   If you haven't installed Rust yet, you can do so by following the instructions on the [Rust website](https://www.rust-lang.org/).

3. **Build the Project**

   Ensure that you can build the project successfully.

   ```sh
   cargo build
   ```

## Making Changes

1. **Create a Branch**

   Create a new branch for your work. Use a descriptive name for the branch.

   ```sh
   git checkout -b my-feature-branch
   ```

2. **Make Your Changes**

   Make your changes in the appropriate crate(s) within the `crates` directory. The projects under [`examples/`](/examples) are runnable pipelines; use one of those to try a change end to end.

3. **Format Your Code**

   Ensure that your code is properly formatted. `.rustfmt.toml` sets nightly-only rustfmt options, so formatting needs a nightly toolchain, and CI pins an exact one. A different nightly reformats differently and the check then fails, so use the pinned one:

   ```sh
   rustup toolchain install nightly-2026-02-25 --component rustfmt
   cargo +nightly-2026-02-25 fmt --all
   ```

4. **Run Clippy**

   Run Clippy to catch common mistakes and ensure code quality. CI runs exactly this, and the feature flag matters: the experimental account parser is off by default and its code is only linted when it is on.

   ```sh
   cargo clippy --all-targets --tests --no-deps \
     --features shipstern-kafka-sink/experimental-account-parser -- -Dwarnings
   ```

## Running Tests

Before submitting your changes, make sure all tests pass. CI runs four suites, and a bare `cargo test` covers only the first, because the other three are feature-gated or live in the excluded workspace:

```sh
cargo test --workspace --lib --tests
cargo test -p shipstern-proto --lib --features parser,stream
cargo test -p shipstern-kafka-sink --features experimental-account-parser --lib --tests
cargo test --tests --manifest-path tests/proc-macro-events/Cargo.toml
```

Parser tests fetch fixture data over RPC. See [Running Tests](/README.md#running-tests) in the README for pointing them at your own endpoint with `RPC_ENDPOINT`.

## Submitting Changes

1. **Commit Your Changes**

   Commit your changes with a descriptive commit message.

   ```sh
   git add .
   git commit -m "Add my new feature"
   ```

2. **Push Your Changes**

   Push your changes to your fork.

   ```sh
   git push origin my-feature-branch
   ```

3. **Open a Pull Request**

Go to the repository on GitHub and open a pull request. Provide a clear description of the changes you have made and the problem they solve.

## Releasing

Version numbers appear in prose as well as in manifests, and the published
snippets have to match the crates that were actually released. A release bumps:

- `Cargo.toml` and `Cargo.lock`
- `CHANGELOG.md`
- `README.md`, the dependency snippet under *Codegen Macro*
- `crates/proc-macro/README.md`, the dependency snippet under *Usage*
- `docs/codama-parser-generation.md`, the dependency snippet in the quick start
- `docs/codama-parser-walkthrough.md`, the version table and the dependency
  snippets in *Set up the consuming crate*

## Code of Conduct

Please note that this project is released with a [`CODE_OF_CONDUCT.md`](/CODE_OF_CONDUCT.md). By participating in this project you agree to abide by its terms.

---

Thank you for contributing to our project! If you have any questions, feel free to ask. We appreciate your efforts in improving the project.
