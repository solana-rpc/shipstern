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

To contribute to this project, you'll need to have Rust installed on your machine. This project builds successfully with the latest **stable** Rust, and also with the latest **nightly**. We also use a workspace structure with multiple crates.

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

   Make your changes in the appropriate crate(s) within the `crates` directory. Use the `crates/test` crate to try out and verify your changes.

3. **Format Your Code**

   Ensure that your code is properly formatted. We use `rustfmt` with nightly toolchain for formatting.

   ```sh
   cargo +nightly fmt --all
   ```

4. **Run Clippy**

   Run Clippy to catch common mistakes and ensure code quality.

   ```sh
   cargo clippy --all-targets --tests -- -Dwarnings
   ```

## Running Tests

Before submitting your changes, make sure all tests pass.

```sh
cargo test
```

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
