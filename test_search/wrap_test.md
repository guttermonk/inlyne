# Test Document for Wrapped Lines and Code Blocks

## Long Lines That Should Wrap

This is a very long line with the word test appearing multiple times throughout. The purpose is to test whether the search highlighting works correctly when the word test wraps to the next line. We want to make sure that when test appears at the end of a line and wraps, or when test is split across lines, the highlighting still works properly. This line contains test multiple times to test the wrapping behavior.

Another long paragraph here with test scattered throughout to ensure that the highlighting system can handle test when it appears in different positions relative to line wraps. The word test should be highlighted correctly regardless of where it falls in the wrapped text. Let's add more test instances to really test this thoroughly.

## Code Blocks with Search Terms

### Simple Code Block

```python
def test_function():
    """This is a test function"""
    test_variable = "test"
    return f"This is a test: {test_variable}"
```

### Longer Code Block

```rust
fn main() {
    let test = "test string";
    println!("Running test");
    
    // This is a test comment
    for i in 0..10 {
        println!("Test iteration: {}", i);
    }
    
    test_helper();
}

fn test_helper() {
    // Another test function
    let mut test_vec = vec!["test", "test", "test"];
    test_vec.push("test");
}
```

### JavaScript Code

```javascript
function runTest() {
    const test = "test value";
    console.log(`This is a test: ${test}`);
    
    // Test loop
    for (let test_i = 0; test_i < 10; test_i++) {
        console.log("test", test_i);
    }
    
    return test;
}

// Test class
class TestClass {
    constructor() {
        this.test = "test";
    }
    
    testMethod() {
        return this.test;
    }
}
```

## Mixed Content

Here's some regular text with test in it, followed by a code block:

```bash
#!/bin/bash
echo "Running test script"
test -f "test.txt" && echo "test file exists"
TEST_VAR="test"
echo "Test variable: $TEST_VAR"
```

And then more regular text with test after the code block.

## Inline Code

This paragraph contains `test` in inline code, as well as regular test text. We should see highlighting in both cases: `another test here` and test without backticks.

## Very Long Line in Code Block

```python
# This is a very long comment line in a code block that contains the word test multiple times and should wrap when displayed. We want to verify that test is highlighted correctly even in wrapped code block lines. Adding more test instances to make sure the line definitely wraps: test test test test test.
def long_test_function_with_very_long_name_to_test_wrapping_in_code_blocks():
    test_variable_with_extremely_long_name = "This is a test string that is quite long to test wrapping"
    return test_variable_with_extremely_long_name
```

## Table with Long Content

| Column 1 | Column 2 | Column 3 |
|----------|----------|----------|
| This is a long cell with test that might wrap | Another test cell | test |
| test at the beginning | test in the middle of some text | test at the end |
| A very long cell content with multiple occurrences of the word test throughout to verify that test highlighting works correctly in table cells that wrap to multiple lines | test | More test content |

## Edge Cases

### Test at Line Boundaries

This line ends with test
test starts this line.

This line has test
right at the boundary where wrapping might occur depending on window width.

### Multiple Tests in Sequence

test test test test test test test test test test test test test test test test test test test test test test test test test test test test test test test test test test test test test test

### Unicode and Test

Test with émojis: 😊 test 😊
Test with accénts: tést, tëst, tèst
Test with other scripts: 日本語test中文test한글test

## Conclusion

This document should thoroughly test:
1. Search highlighting in wrapped lines
2. Search highlighting in code blocks
3. Search highlighting when test spans line boundaries
4. Search highlighting in tables
5. Search highlighting with inline code