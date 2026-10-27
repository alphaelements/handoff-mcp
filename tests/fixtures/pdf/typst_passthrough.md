# Typst パススルーのテスト

通常の Markdown 段落。この段落は通常どおり変換されること。

## 数式

```typst
$ f_c = 1 / (2 pi R C) $
```

## 高度な表

```typst
#table(
  columns: (auto, 1fr, auto),
  inset: 6pt,
  table.header([*部品*], [*説明*], [*値*]),
  [R1], [プルアップ抵抗], [10 kΩ],
  [C1], [デカップリング], [100 nF],
  table.cell(colspan: 3)[合計 2 点],
)
```

## 図と相互参照

```typst
#figure(
  rect(width: 6cm, height: 2cm, fill: luma(230))[ブロック図],
  caption: [システム構成],
) <fig-system>

@fig-system を参照。
```

## 改ページ

```typst
#pagebreak()
```

パススルー後の通常段落。Typst ブロックの外側は影響を受けないこと。
