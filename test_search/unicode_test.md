# Unicode and Complex Text Search Test

## Accented Characters

Test café, naïve, résumé, Zürich, façade, piñata

Search for: cafe, naive, resume, Zurich, facade, pinata (without accents)
These should match if normalization is working correctly.

## Emoji and Complex Unicode

Test with emoji: 👨‍👩‍👧‍👦 family test 🎉
Test with skin tone modifiers: 👋🏽 test 👋🏿
Test with combining characters: é (e + combining acute) test
Test with zero-width joiners: 👨‍💻 developer test

## Different Cases

TEST test Test TeSt tEsT - all variations of "test"

## Ligatures and Special Characters

Find test in: ﬁtest, ﬂtest (fi and fl ligatures if font supports)
Mathematical: ∑test, ∫test, √test
Arrows: →test, ←test, ↔test

## Japanese, Chinese, Korean

Japanese: テスト (tesuto in katakana) test
Chinese: 测试 (cèshì) test  
Korean: 테스트 (teseuteu) test

## Right-to-Left Text

Arabic: اختبار test
Hebrew: מבחן test

## Grapheme Clusters

Complex grapheme: ಠ_ಠ test
Combining diacritics: ẽ́ test (e + tilde + acute)
Regional indicators: 🇺🇸 test 🇬🇧

## Long Words with Special Characters

supercalifragilisticexpialidocious-test
pneumonoultramicroscopicsilicovolcanoconiosis-test
Rindfleischetikettierungsüberwachungsaufgabenübertragungsgesetz-test

## Tables with Unicode

| Column | Test | Unicode |
|--------|------|---------|
| café   | test | ✓       |
| naïve  | test | ✗       |
| 日本語 | test | 😊      |
| test   | café | résumé  |

## Code Blocks with Unicode

```python
def test_café():
    """Test function with unicode in name"""
    emoji = "😊 test"
    japanese = "テスト test"
    return f"{emoji} {japanese}"
```

## Mixed Direction Text

This line has English test مع عربي test עם עברית test again.

## Zero-Width Characters

Test with zero‌width‌non‌joiner test
Test with zero​width​space test
Test with soft­hyphen test

## Normalized vs Decomposed

Café (composed: é) test
Café (decomposed: e + ́) test
Both should match when searching for "cafe test"

## Currency and Symbols

€100 test, $50 test, ¥1000 test, £25 test
© test ® test ™ test ℠ test

## Subscript and Superscript

H₂O test, x² test, 10³ test
ₐtest ᵇtest ᶜtest

## Box Drawing

┌─────────┐
│  test   │
├─────────┤
│  test   │
└─────────┘

## Mathematical Symbols

∀x test ∃y test ∈ test ∉ test
α test β test γ test δ test
∞ test ≈ test ≠ test ≤ test

## Conclusion

All instances of "test" above should be searchable and highlightable correctly, regardless of:
- Surrounding Unicode characters
- Text direction
- Normalization form
- Case variations
- Special characters
- Grapheme clusters