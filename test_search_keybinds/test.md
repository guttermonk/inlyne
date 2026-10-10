# Test Document for Search Keybind Testing

## Introduction

This document is designed to test the search functionality and keybind behavior in Inlyne. It contains various sections with repeated words and patterns to make searching effective.

## Section 1: Lorem Ipsum

Lorem ipsum dolor sit amet, consectetur adipiscing elit. The word "test" appears multiple times in this document. This is a test sentence. Another test here. And yet another test to find.

Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Test cases are important. We need to test everything thoroughly.

## Section 2: Code Examples

Here's some code with searchable content:

```python
def test_function():
    print("This is a test")
    return "test result"
```

```javascript
function testSearch() {
    console.log("Testing search functionality");
    let test = "test value";
    return test;
}
```

## Section 3: Lists and Tables

### Unordered List
- First test item
- Second test item  
- Third item with test
- Testing fourth item

### Ordered List
1. Test number one
2. Number two test
3. Third test case
4. Fourth testing scenario

### Table with Test Data

| Header 1 | Header 2 | Header 3 |
|----------|----------|----------|
| test 1   | data     | value    |
| item     | test 2   | result   |
| sample   | info     | test 3   |
| test 4   | test 5   | test 6   |

## Section 4: Long Paragraph

This is a longer paragraph designed to test scrolling behavior during search. The word "test" appears multiple times throughout this paragraph. When you search for "test", you should be able to navigate between all instances using the navigation keys. This test ensures that the search functionality works correctly. Additionally, we want to test that other keybinds (like j/k for scrolling, or Page Up/Down) are properly disabled during search mode. Only search-related keys should work: Enter, Tab, n (after Enter), Shift+n (after Enter), Shift+Tab, and Escape to cancel. This is a critical test for the user experience.

## Section 5: Nested Content

### Subsection 5.1
Test content in subsection.

#### Subsubsection 5.1.1
Deep test nesting here.

### Subsection 5.2
Another test in different subsection.

## Section 6: Special Characters and Formatting

**Bold test text**

*Italic test text*

***Bold and italic test***

`Inline code with test`

> Blockquote with test content
> Multiple lines of test
> To ensure search works in quotes

## Section 7: Links and References

[Test Link](https://example.com)
[Another test link with text](https://example.org)

## Section 8: Final Test Section

The final section contains more test instances to ensure we have enough search results to properly test navigation. Test appears here. And test is here too. One more test for good measure.

### Edge Cases

- test at the beginning
- in the middle test placement  
- placement at the end test
- TEST in capitals
- TeSt with mixed case
- t e s t with spaces (shouldn't match)

## Conclusion

This document should provide a comprehensive test bed for the search functionality. The word "test" appears numerous times, allowing thorough testing of:

1. Search activation (Ctrl+F or Cmd+F)
2. Typing search queries
3. Navigation between matches (Enter, Tab, n after Enter)
4. Previous match navigation (Shift+Tab, Shift+n after Enter)
5. Search cancellation (Escape)
6. Verification that non-search keybinds are disabled during search

End of test document.