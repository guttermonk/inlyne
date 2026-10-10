# Search Highlighting Test Document

This document contains various text patterns to test the accuracy of search highlighting in Inlyne.

## Simple Text Search

The word "test" appears multiple times in this paragraph. This is a test of the search functionality. We want to test if the highlighting works correctly when the word test appears multiple times.

## Unicode and Special Characters

Here are some special cases:
- Café (with accent)
- 日本語 (Japanese text)
- 😊 Emoji test 😊
- Test with émojis and áccents

## Code Blocks

```rust
fn test_function() {
    let test_variable = "test";
    println!("This is a test");
}
```

Inline code: `test` and `another test` in backticks.

## Table Test

| Column 1 | Column 2 | Column 3 |
|----------|----------|----------|
| test     | data     | test     |
| more     | test     | here     |
| test     | test     | test     |

## Long Lines

This is a very long line that contains the word test somewhere in the middle of it, and we want to make sure that the search highlighting can find and properly highlight the word test even when it appears in a very long line of text that might wrap across multiple rendered lines.

## Nested Structures

> This is a quote block with the word test
> > And a nested quote with test
> > > And another level with test

- List item with test
  - Nested list with test
    - Deeply nested test

## Mixed Content

Here's a paragraph with **bold test**, *italic test*, and ***bold italic test*** formatting.

Also testing with [link text containing test](https://example.com) and another test after the link.

## Edge Cases

testtest (repeated without space)
test test (repeated with space)
Testing (with different case)
TESTING (all caps)
tEsT (mixed case)

## Numbers and Symbols

test123
456test
test-test
test_test
test.test
(test)
[test]
{test}

## Line Boundaries

test
at the beginning of a line.

At the end of a line is test

And test
spans across
multiple lines.

## Conclusion

This document should help test the search highlighting accuracy for various text patterns and edge cases.