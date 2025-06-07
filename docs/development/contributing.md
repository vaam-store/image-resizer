# Contributing

Thank you for considering contributing to the Image Resize Service!

## How to Contribute

We welcome contributions in various forms:

- **Bug Reports**: If you find a bug, please open an issue on GitHub.
- **Feature Requests**: If you have an idea for a new feature, please open an issue to discuss it.
- **Code Contributions**: Pull requests are welcome!
- **Documentation Improvements**: If you find any gaps or errors in the documentation, please let us know or submit a pull request.

## Development Setup

Please refer to the [Installation](../getting-started/installation.md) guide for setting up your local development environment.

## Code Style

This project follows standard Rust coding conventions. Please run `cargo fmt` to format your code before submitting a pull request.

We also use Clippy for linting:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Refer to the `.clippy.toml` file for specific Clippy configurations.

## Commit Messages

Please follow the [Conventional Commits](https://www.conventionalcommits.org/) specification for your commit messages. This helps in automating changelog generation and versioning.

Example:

```
feat: Add support for WEBP output format

This commit introduces the ability to output images in WEBP format.
- Added WEBP encoding option.
- Updated API documentation.
```

## Pull Request Process

1.  **Fork the repository** and create your branch from `main`.
2.  **Make your changes**.
3.  **Add tests** for your changes.
4.  **Ensure all tests pass**: `cargo test`.
5.  **Format your code**: `cargo fmt`.
6.  **Lint your code**: `cargo clippy`.
7.  **Commit your changes** using conventional commit messages.
8.  **Push your branch** to your fork.
9.  **Open a pull request** to the `main` branch of the original repository.

Please provide a clear description of your changes in the pull request.

## Testing

Refer to the [Testing](./testing.md) guide for more details on how to run and write tests.

## License

By contributing, you agree that your contributions will be licensed under the [LICENSE](../about/license.md) file.