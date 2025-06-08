# Testing

This guide explains how to run and write tests for the Image Resize Service.

## Running Tests

To run all tests in the project:

```bash
cargo test
```

This command will execute unit tests, integration tests, and documentation tests.

### Running Specific Tests

- **Run tests for a specific package**:
  ```bash
  cargo test -p <package-name>
  ```
- **Run tests for a specific module**:
  ```bash
  cargo test src/modules/api/mod.rs
  ```
- **Run a specific test function**:
  ```bash
  cargo test my_test_function_name
  ```

### Test Coverage

To generate a test coverage report, you can use tools like `cargo-tarpaulin` or `grcov`.

Example with `cargo-tarpaulin`:

```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Html
```

This will generate an HTML coverage report in `./tarpaulin-report.html`.

## Writing Tests

### Unit Tests

Unit tests are typically placed in the same file as the code they are testing, within a `#[cfg(test)]` module.

Example:

```rust
// src/my_module.rs

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 2), 4);
    }
}
```

### Integration Tests

Integration tests are placed in the `tests/` directory at the root of the crate. Each `.rs` file in this directory is compiled as a separate crate.

Example (`tests/integration_test.rs`):

```rust
use image_resize; // Assuming your crate name is image_resize

#[test]
fn test_some_integration_scenario() {
    // Your integration test logic here
    // For example, call API endpoints and verify responses
}
```

### Test Organization

- **Unit tests**: Test individual functions and modules in isolation.
- **Integration tests**: Test how different parts of the application work together. This often involves testing API endpoints, database interactions, etc.

## Mocking

For testing components that interact with external services (like S3 or databases), consider using mocking libraries or techniques:

- **Mockall**: A powerful mocking library for Rust.
- **Test Doubles**: Implement simple test doubles (stubs, fakes) for dependencies.
- **In-memory implementations**: Use in-memory versions of services (e.g., in-memory storage handler) for testing.

## Best Practices

- Write tests for all new features and bug fixes.
- Keep tests small and focused on a single piece of functionality.
- Ensure tests are independent and can be run in any order.
- Use descriptive test names.
- Test edge cases and error conditions.
- Aim for high test coverage.