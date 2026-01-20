---
name: healthy-mind-test-generator
description: "Generate test files for the Healthy Mind project following established patterns including 76-character section headers, AAA pattern with comments, test fixtures, and database initialization. Use when adding tests for new features, server functions, or components. Triggers: add tests, create test file, generate tests for, write tests."
---

# Healthy Mind Test Generator

Generate tests following project-specific patterns.

## Workflow

1. Determine test requirements:
   - What module/function to test?
   - Async or sync tests?
   - Database needed?
   - Test categories (unit, integration, security)

2. Generate test file with:
   - Proper imports
   - Section headers (76 `=` characters)
   - Fixture functions
   - AAA-patterned tests

3. Place in `api/tests/` for backend tests

## Test Patterns

See `references/test-patterns.md` for complete patterns.

## Quick Reference

**Section Header (76 chars):**
```rust
// ============================================================================
// Section Name Here
// ============================================================================
```

**Fixture Function:**
```rust
async fn setup_test_auth() -> AuthManager {
    let db_url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://user:password@localhost/healthy_mind_db_test".to_string());
    let pool = init_test_db(&db_url).await.unwrap();
    AuthManager::new(pool)
}
```

**AAA Test:**
```rust
#[tokio::test]
async fn test_operation_scenario() {
    // Arrange
    let auth = setup_test_auth().await;
    let user = create_test_user(&auth).await;

    // Act
    let result = auth.some_operation(user.id).await;

    // Assert
    assert!(result.is_ok());
    assert_eq!(result.unwrap().field, expected_value);
}
```

**Sync Test:**
```rust
#[test]
fn test_validation_success() {
    // Arrange
    let input = "valid_input";

    // Act
    let result = validate_input(input);

    // Assert
    assert!(result.is_ok());
}
```

## Test Naming Convention

`test_<operation>_<scenario>`

Examples:
- `test_register_user_success`
- `test_login_wrong_password`
- `test_validate_session_expired`
