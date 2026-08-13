# lopdf PDF 优化器总结

日期：2026-08-14

## 结论

已在 `lopdf` 中实现独立的通用 PDF 优化器，不依赖或复制 MuPDF/AGPL 源码。实现覆盖写出、文档结构、内容流、字体、图片五个层面，默认以安全为先：无法确认转换安全时保留原对象，并在报告中记录跳过原因。

本次不是继续按单个 PDF 追问题，而是按 MuPDF 的完整保存链路建立能力矩阵：writer、garbage/dedup、content clean/sanitize、font subset、image rewrite。对照后补齐了安全且通用的 XRef stream 最优字段宽度与 Flate 压缩、最短字符串表示、Type3 CharProcs 内容流最小化。三个固定基准样例均小于同配置 MuPDF；两个文本样例保持文本和像素级完全一致。

## 代码入口

- `src/optimizer.rs`
  - `Document::optimize`
  - 文档流压缩
  - 页面、Form、tiling Pattern 和 Type3 CharProcs 内容流规范化与最小化
  - 未引用对象清理
  - 内容感知对象去重
  - 对象编号重排
  - 图片使用位置和 DPI 分析
  - 图片重采样与重编码
- `src/optimizer/fonts.rs`
  - Identity-H/Identity-V 的 CIDFontType2 TrueType 字体子集化
  - 生成 `CIDToGIDMap`，不改写原始文本 CID
  - 聚合共享字体程序的使用字形
  - 对 AcroForm 字体和不完整内容分析采取保护策略
  - 在子集化前兼容表目录未按 tag 排序的现实 TrueType 字体
- `src/writer.rs`
  - XRef stream 按实际 offset/generation/object-stream index 计算最小 `/W`
  - 仅在包含 `/Filter` 开销后仍更小时使用 Flate
  - 内容流字符串按字节值选择更短的 literal/hex 写法
- `examples/optimize_pdf.rs`
  - 有界加载
  - 最大压缩配置
  - Object Stream/XRef Stream 输出
  - 输出结构、字体、图片优化报告
- `tests/optimizer.rs`
  - 内容流最小化、最短字符串表示、Type3 CharProcs 与 Inline Image 保护
  - 结构去重
  - 嵌套 Form 的 CTM/DPI 分析
  - 共享图片的最低分辨率保护
  - Base image/soft mask 保护
- `tests/font_optimizer.rs`
  - CID 字体子集化
  - 文本 CID 不被改写
  - 非排序 SFNT 表目录回归测试

## API 与 Feature

最大压缩配置：

```rust
let report = document.optimize(&lopdf::OptimizeOptions::maximum_compression())?;
```

Feature：

- `embed_image`：图片解码、重采样和 JPEG/Flate 重编码
- `font_subsetting`：TrueType CID 字体子集化

报告类型：

- `StructureOptimizationReport`
- `FontOptimizationReport`
- `ImageOptimizationReport`
- `OptimizeReport`

## MuPDF 系统能力矩阵

对照入口：`source/pdf/pdf-write.c`、`pdf-clean.c`、`pdf-op-filter.c`、`pdf-subset.c`、`pdf-image-rewriter.c`。只分析行为与边界，未复制 AGPL 实现。

| Pass | MuPDF | lopdf 当前状态 | 决策 |
|---|---|---|---|
| mark/sweep、xref compact/renumber | 支持，非法引用置 null | 支持 prune + renumber | 已覆盖 |
| 对象去重 | 浅层与 deep-reference 两档，固定点迭代，跳过 Page | 内容哈希/相等判断，固定点迭代，跳过 Page | 当前安全；deep-reference 去重收益需单独语料证明 |
| Object Stream | 重建并 Flate | 支持 | 已覆盖 |
| XRef Stream | 动态 `/W`、Flate | 动态 `/W`、收益为正时 Flate | 本次补齐 |
| 普通 stream | 短流跳过；Flate/Brotli；1-bit 可试 CCITT G4 | Flate；计入 `/Filter` 19 B 开销；只保留更小结果 | Flate 已覆盖；Brotli/CCITT 暂不引入 |
| content clean | 页面、Form、Pattern、soft mask、annotation AP、Type3；最小空白 | 页面、所有 Form、tiling Pattern、Type3；严格解析、64 MiB 上限、Inline Image 跳过 | 本次补齐 Type3 与最短 string |
| content sanitize | 平衡 q/Q、删除重复 graphics-state/color 操作、重建已用 Resources | 未实现 | 高价值但语义面大；需独立 graphics-state processor |
| font subset | simple/CID TrueType、CFF/OpenType、非 Identity CMap；同字体程序先去重 | Type0 + Identity-H/V + CIDFontType2 + FontFile2 | 继续保守；下一优先级为 simple TrueType |
| image rewrite | JPEG/JPX/Flate/Fax；灰/彩/二值；可色彩转换；Inline Image | JPEG/Flate/LZW/ASCII；灰/彩/二值；共享 DPI/嵌套 Form；mask/JPX/CMYK/Inline 跳过 | 已覆盖常见安全子集 |
| page select/vectorize/appearance/encryption | 工具级附加能力 | 非优化器职责或已有独立 API | 不纳入压缩优化器 |

优先级结论：

1. 已实现：XRef stream、最短字符串表示、Type3 CharProcs。
2. 下一安全增益：simple TrueType 子集化、font-program digest 去重。
3. 需要更强验证后再做：graphics-state sanitize、非 Identity CMap/CFF、Inline Image。
4. 不为追求尺寸强制做：CMYK/DeviceN 色彩转换、mask 重写、JPX/JBIG2/CCITT 编码。

## 图片处理范围

已支持安全重写：

- JPEG、Flate、LZW、ASCII85、ASCIIHex、RunLength 图片流
- DeviceGray、DeviceRGB、CalGray、CalRGB
- 1 通道或 3 通道 ICCBased 色彩空间
- 1 bit 与 8 bit 图片
- 嵌套 Form、继承 Resources、CTM、UserUnit、Pattern、soft mask 中的图片使用分析
- 同一 Image XObject 多处复用时，按最低实际 DPI 决定是否降采样
- 仅在结果更小时替换图片流

当前保守跳过：

- JPX/JPEG 2000
- CMYK、DeviceN、Separation 和不支持的 ICC 色彩空间
- Inline Image
- 带 `Mask`/`SMask` 的 base image 及其 mask image
- 不支持的 filter、位深、尺寸或解码失败的图片

所有跳过均通过 `ImageSkipReason` 计数，不伪造优化成功。

## 字体处理范围

当前只对子集化条件明确的字体执行转换：

- Type0 + `Identity-H`/`Identity-V`
- 单个 `CIDFontType2` descendant
- `CIDToGIDMap /Identity` 或缺省
- 可读取的 `FontFile2`
- SFNT 表目录可以在真实文件中未按 tag 排序；优化器会在内存中规范化目录后再子集化
- 能完整分析页面、Form、Pattern、soft mask 和页面注释外观流中的文本内容

子集化会：

1. 收集实际使用的 CID/GID。
2. 使用 `subsetter` 生成连续 GID 的字体子集。
3. 生成新的 `CIDToGIDMap`，保持原始 PDF 文本编码不变。
4. 保持原始内容流 CID 不变。
5. 只有字体程序变小时才替换。

无法安全处理的字体会保留原内容，并报告跳过原因。

## 样例结果

输入文件位于仓库根目录：

- `20260507G0410027-d6f86eac.pdf`
- `Scanned Document.pdf`

| 样例 | 原始大小 | 此实现 | 文件缩减 | MuPDF 系统对照 |
|---|---:|---:|---:|---:|
| 文本/矢量 PDF | 77,969 B | **41,443 B** | **46.85%** | 42,754 B |
| 扫描 PDF | 1,646,883 B | **658,358 B** | **60.02%** | 740,561 B |
| 用户 `1.pdf` | 82,151 B | **41,378 B** | **49.63%** | 42,188 B |

### 文本/矢量 PDF

- `mutool draw -F txt` 输出与原文件字节一致。
- 144 DPI 渲染使用 ImageMagick `compare -metric AE`：**0 differing pixels**。
- 2 个 Type0 字体均完成子集化。
- 字体程序压缩前后：62,090 B → 28,495 B。

### 扫描 PDF

- 原始图片：3712×2698，JPEG，ICC，8 bit。
- 有效页面使用 DPI 为 72，因此没有错误降采样，只进行了 JPEG 重编码。
- 72 DPI 渲染 MAE：`275.457 (0.0042032)`，约 **1.072/255**。
- MuPDF 系统对照：740,561 B，72 DPI MAE `415.051 (0.00633327)`。
- 当前实现小 82,203 B，且本样例渲染误差更低。

### 用户报告的 `1.pdf`

- 原始文件：82,151 B。
- 修复前 lopdf：81,453 B，仅缩减 0.85%，字体报告为 `SubsetFailed`。
- 最终 lopdf：**41,378 B**，缩减 **49.63%**。
- iLovePDF：57,151 B；MuPDF 系统对照：42,188 B。当前结果比 iLovePDF 小 15,773 B，比 MuPDF 小 810 B。
- 字体程序流：69,128 B → 29,116 B，节省 40,012 B。
- 内容流 minify 后再压缩：报告节省 1,336 B。
- `mutool draw -F txt` 输出与原文件字节一致。
- 150 DPI 渲染：**0 differing pixels**。

根因不是 CID 映射。该 PDF 的内嵌 TrueType 表目录是逆序的；`subsetter 0.2.6` 使用二分查找并假定表目录已按 tag 排序，因此找不到 `glyf` 表并返回 `UnknownKind`。MuPDF 的 `source/fitz/subset-ttf.c::find_table` 逐项扫描表目录，所以能处理该文件。

lopdf 在调用 `subsetter` 前检查 SFNT 表目录：只有目录未排序时才复制字体数据并排序 16 字节目录记录，表偏移和表内容保持不变。内容优化覆盖页面、Form、tiling Pattern 和 Type3 CharProcs；字符串值不变，只在 literal 与 hex 之间选择更短表示。无法解析、超过 64 MiB 或含 Inline Image 的流保持不变。

## 性能

Release 构建，使用 `/usr/bin/time -l`：

| 样例 | 优化器耗时 | 最大常驻内存 |
|---|---:|---:|
| 文本/矢量 PDF | 0.005 s | 12.0 MB |
| 扫描 PDF | 0.065 s | 104.3 MB |

扫描图片解码和 JPEG 重编码是主要内存与 CPU 开销。

## 固定语料验证

- 文本/矢量 PDF：41,443 B vs MuPDF 42,754 B；144 DPI **0 differing pixels**；文本字节一致。
- 用户 `1.pdf`：41,378 B vs MuPDF 42,188 B；150 DPI **0 differing pixels**；文本字节一致。
- 扫描 PDF：658,358 B vs MuPDF 740,561 B；72 DPI MAE `275.457 (0.0042032)`；同参数 MuPDF MAE `415.051 (0.00633327)`。

此前 POC 的 6 个风险样本也已覆盖 JPEG XObject、JPX、soft mask、alpha、CMYK ICC、共享 Image XObject；不支持的表示保持原对象。
## 验证命令

```bash
cargo test --all-features
cargo clippy --all-features --lib --example optimize_pdf --test optimizer --test font_optimizer --test save_options_xref_streams_test -- -D warnings
```
最后一次完整测试结果：**262 passed，0 failed**；上述 clippy 命令 0 warnings。

## 后续覆盖边界

目前仍未实现以下有损或格式转换能力：

- content sanitize：graphics-state 平衡、重复状态操作删除、已用 Resources 重建
- simple TrueType、非 Identity Type0/CID 映射、CFF/OpenType 子集化
- font-program digest 去重
- JPX/JPEG 2000、CCITT/JBIG2 重编码
- CMYK/DeviceN 到 RGB 的色彩管理转换
- 带 mask 图片与 Inline Image 的安全重写

这些能力应继续保持“解码、转换、渲染验证、失败即保留”的策略，不能通过强制转换来换取表面文件缩减。
