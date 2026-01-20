# Test Patterns Reference

## File Structure

```rust
//! Tests for {module_name}

use api::auth::AuthManager;
use api::models::{User, UserRole};
use api::init_test_db;

// ============================================================================
// Test Fixtures
// ============================================================================

async fn setup_test_auth() -> AuthManager {
    let db_url = std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://user:password@localhost/healthy_mind_db_test".to_string());
    let pool = init_test_db(&db_url).await.unwrap();
    AuthManager::new(pool)
}

async fn create_test_user(auth: &AuthManager) -> User {
    let (user, _profile) = auth
        .register_user(
            "testuser".to_string(),
            "test@example.com".to_string(),
            "SecurePass123".to_string(),
            UserRole::Patient,
            "Test".to_string(),
            "User".to_string(),
        )
        .await
        .unwrap();
    user
}

// ============================================================================
// Unit Tests - {Category}
// ============================================================================

#[tokio::test]
async fn test_feature_success() {
    // Arrange
    let auth = setup_test_auth().await;

    // Act
    let result = auth.some_feature().await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_feature_error_case() {
    // Arrange
    let auth = setup_test_auth().await;

    // Act
    let result = auth.some_feature_with_bad_input().await;

    // Assert
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("expected error"));
}

// ============================================================================
// Integration Tests
// ============================================================================

#[tokio::test]
async fn test_full_workflow() {
    // Arrange
    let auth = setup_test_auth().await;

    // Act & Assert - Step 1
    let user = create_test_user(&auth).await;
    assert_eq!(user.username, "testuser");

    // Act & Assert - Step 2
    let (logged_in, session) = auth
        .login_user("testuser".to_string(), "SecurePass123".to_string())
        .await
        .unwrap();
    assert_eq!(logged_in.id, user.id);

    // Act & Assert - Step 3
    let logged_out = auth.logout_user(session.id).await.unwrap();
    assert!(logged_out);
}
```

## Multiple Test Cases Pattern

```rust
#[test]
fn test_validation_multiple_valid_inputs() {
    // Arrange
    let valid_inputs = vec!["Input1", "Input2", "Input3"];

    // Act & Assert
    for input in valid_inputs {
        let result = validate(input);
        assert!(result.is_ok(), "Input '{}' should be valid", input);
    }
}
```

## Error Message Validation

```rust
#[tokio::test]
async fn test_operation_specific_error() {
    // Arrange
    let auth = setup_test_auth().await;

    // Act
    let result = auth.operation_that_fails().await;

    // Assert
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("specific error message"));
}
```

## Key Points

1. **Section headers**: Exactly 76 `=` characters
2. **AAA comments**: Always include `// Arrange`, `// Act`, `// Assert`
3. **Fixture functions**: Reusable setup at top of file
4. **Test naming**: `test_<operation>_<scenario>`
5. **Async tests**: Use `#[tokio::test]`
6. **Database tests**: Use `init_test_db()` in fixtures
