# エッジケース

## 存在しない画像参照

![存在しない画像](assets/does-not-exist.png)

## 不正な Mermaid 構文

```mermaid
this is not a valid mermaid diagram
  -->>> ???
```

## 空の Mermaid ブロック

```mermaid
```

## 空の Typst ブロック

```typst
```

## 見出しのみの節

## 特殊文字

Typst の特殊文字: # $ @ * _ ` < > \ [ ] がエスケープされること。

### 空セクション直前

最後の段落。
