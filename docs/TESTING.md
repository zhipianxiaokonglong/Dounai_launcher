# 测试方法学

以下记录启动器测试中三个容易让测试本身失效的陷阱。这些不是产品缺陷，但会让“测试通过”变成假象。

## 增量编译缓存让结果失真

变异测试（故意注入缺陷，验证测试能否抓住）出现过以下序列：

1. 注入缺陷，测试失败。
2. 还原代码，测试仍失败。
3. 再次注入，测试失败。
4. 清理该库的构建产物后，测试通过。

原因是还原 `security.rs` 后，增量编译没有重新编译该库。测试链接的仍是注入缺陷后的旧 `rlib`。清理移除了 4.1 GiB 构建产物，行为随即恢复。

这会制造假阳性，也会让“修好缺陷但测试仍失败”被误读。

警告：做变异测试或修改安全关键代码后，先清理构建产物再验证。清理会删除本地编译缓存，重新构建耗时较长。

```powershell
cargo clean -p dounai-launcher
cargo test --all-targets
```

CI 每个 job 都是干净环境，不受此影响。

## 断言过弱会掩盖防线退化

`fuzz_safe_join_never_escapes_destination` 只断言被接受的路径仍在目标目录内。删掉 `safe_join` 里 `ParentDir` 的显式拒绝后，这条测试仍通过，因为末尾的包含性复核把越界兜住了。

纵深防御是双保险。第二道还在时，不能默许第一道失效。否则防线少一层，测试不会报警。

应对方式是新增 `fuzz_attack_strings_are_explicitly_rejected_by_design`。该测试要求攻击串返回语义明确的错误类型（`Traversal`、`Absolute`、`ReservedName`），而不是随便报错就算通过。注入缺陷后这条会准确失败，其余 7 条仍通过，说明该测试守的是那道具体的防线。

## fuzz 覆盖

`tests/fuzz_invariants.rs` 有 8 项测试，约 4300 次迭代。

| 不变量 | 覆盖方式 | 迭代数 |
|---|---|---|
| `safe_join` 不越界 | 拼路径池片段、随机分隔符、随机字符 | 2000 |
| 攻击串按设计拒绝 | 显式枚举 14 个真实攻击串 | 14 |
| 解压不越界、不 panic | 纯随机字节喂给 zip 解析与解压 | 300 |
| 恶意条目整体拒绝 | 14 个恶意条目各构造一个混合包 | 14 |
| 正常条目不被误拒 | 5 个正常路径反向验证 | 5 |
| 清单解析不 panic | 随机字节 | 500 |
| 清单结构化变异 | 以真实清单为基底做翻转、截断、插入 | 300 |
| base64 解码不 panic | 随机可打印字符 | 800 |

不用外部 fuzz 引擎的原因：

1. 不需要 nightly 与额外工具链，CI 直接可跑。
2. 种子固定（`0xC0FFEE`、`0xDEADBEEF`），失败可复现。
3. 覆盖真实攻击面（zip 头、路径字段、JSON 结构），而非纯随机噪声。

代价是不追求覆盖率最大化，只保证已知攻击面的不变量成立。

fuzz 最常见的失败模式是“全部拒绝”也能通过。因此有一条 `fuzz_good_entries_still_accepted`，确保正常路径不被误拒。

## 复现命令

```powershell
cd src-tauri
cargo test --all-targets
cargo test --test fuzz_invariants
```

做变异测试时先清理：

```powershell
cargo clean -p dounai-launcher
cargo test --test fuzz_invariants
```

注意：M1 抽出 `launcher-core` 后，包名会变化。命令里的 `-p dounai-launcher` 需随新包名调整。
