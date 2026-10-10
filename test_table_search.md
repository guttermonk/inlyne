# Test Document with Table for Search

This document tests search functionality with tables and regular text.

## Regular Text Section

Here's some regular text with the word **apple** and **banana** for searching.

## Table Section

Here's a table with searchable content:

| Fruit | Color | Taste | Season |
|-------|-------|-------|--------|
| Apple | Red | Sweet | Fall |
| Banana | Yellow | Sweet | Year-round |
| Cherry | Red | Tart | Summer |
| Date | Brown | Sweet | Winter |
| Elderberry | Purple | Tart | Fall |

## Mixed Content

Some more text after the table. The word **apple** appears here again.

### Nested Table

| Item | Description | Price |
|------|-------------|-------|
| Apple Pie | Delicious dessert with apples | $5.99 |
| Banana Bread | Sweet bread with bananas | $4.99 |
| Cherry Tart | Tart pastry with cherries | $6.99 |

## List with Fruits

- Fresh **apple** from the orchard
- Ripe **banana** from tropical regions  
- Sweet **cherry** from the tree

## Code Block

```python
fruits = ["apple", "banana", "cherry"]
for fruit in fruits:
    print(f"I like {fruit}")
```

## Search Test Instructions

1. Press **Ctrl+F** to activate search
2. Try searching for:
   - "apple" - should find it in text, tables, and lists
   - "banana" - appears in multiple places
   - "cherry" - in table and list
   - "sweet" - appears in table descriptions
   - "red" - appears in color column
3. Use **n** or **Enter** to navigate forward
4. Use **Shift+n** to navigate backward

The search should find matches in:
- Regular paragraphs
- Table cells
- List items
- Headers
- Bold text

Note: Code blocks should also be searchable.