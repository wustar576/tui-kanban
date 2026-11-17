# tui-kanban

[English](README.md) | 繁體中文

`kb` 是一個終端機看板，用來觀看 AI coding agent 的工作。agent 在工作時以指令 `kb` 寫入看板；
你在只讀的終端機畫面 `kb tui` 裡觀看。它經由 hook 接上 Claude Code，資料存在本機的
PostgreSQL。

## 為什麼

coding agent 執行長的任務時，它的規劃與進度散在很長的對話紀錄裡。想知道它做了什麼、為什麼
做某個決定、卡在哪裡，就得讀那份紀錄。`todo.md` 的勾選清單只表達完成或未完成；它不表達
階段、阻擋、一件工作的處理過程，以及工作之間的關係。

tui-kanban 把這些資訊放到看板上：

- agent 把工作規劃成泳道與 epic 裡的卡片，邊做邊移動卡片，並把紀錄、決定與結果寫在卡片上。
- 你觀看。一個畫面就看得出目前的階段、什麼被阻擋、最近改了什麼，不必讀對話紀錄。
- Claude Code 的 hook 連接兩邊：把你的回答與留言送到 agent，並在每一輪結束時以一組守則檢查
  agent 的行為。

看板由你開始，不由 agent 開始。沒有看板的 session 不受影響：在你輸入 `/kb:kb` 之前，hook
不向 agent 提到看板。

## 你會看到什麼

看板畫面的示意圖：

```
 Acme_DevOps › 修正登入逾時 › 3f2a9c1e…                       提問 1  討論 1  ● 已連線
┌ EPIC ──────────────┐┌ 待辦 (2) ──────────────┐┌ 處理中 (1) ────────────┐┌ 完成 3 ──┐
│ ✓ P0 重現      2/2 ││┌──────────────────────┐││┏━━━━━━━━━━━━━━━━━━━━━━┓││ #1 重現… │
│   P1 修正  1/4 +1  │││ #6 token 的有效期    │││┃▶#5 閒置時更新 token  ┃││ #2 收集… │
│   P2 驗證      0/2 │││ P1  提問             │││┃ P1  衍生             ┃││ #3 撰寫… │
│                    ││└──────────────────────┘││┃ ↳ 延伸自 #3          ┃││          │
│                    ││┌──────────────────────┐││┗━━━━━━━━━━━━━━━━━━━━━━┛││          │
│                    │││ #7 壓力測試          │││                        ││          │
│                    │││ P2  討論             │││                        ││          │
│                    ││└──────────────────────┘││                        ││          │
└────────────────────┘└────────────────────────┘└────────────────────────┘└──────────┘
 11:20 #3 完成
 11:19 #6 提問：token 的有效期要改成多久？
 11:18 #5 移到「處理中」
 ←→ 泳道/側欄  ↑↓ 卡片  Enter 開啟  a 提問  i 留言  e epic  o 排序  s 側欄  q 離開
```

agent 寫入後，畫面在 1 秒內重畫。你可以用它做這些事：

1. **追蹤規劃與進度。** 泳道表示進度；側欄的 epic 表示階段，附上 `done/total`。每張卡片
   標示它的來源：與你約定過、還在討論，或 agent 在執行中自己增加（附上它延伸自哪張卡片）。
   剛變動的卡片外框以綠色顯示 2 秒。
2. **看到決定與它的依據。** agent 把決定寫成卡片的紀錄，並可以寫明這個決定依據什麼：另一張
   卡片、一個提問、一則留言，或你在對話裡的指示。卡片畫面列出依據，也反過來列出這張卡片
   引出的決定；`Enter` 在它們之間跳轉。
3. **在 TUI 裡回答 agent 的提問。** agent 不停下來等待，而是把附有選項的問題寫在卡片上，
   繼續處理其他卡片。按 `a` 跳到下一個待回答的提問，勾選選項或打字回答，按 `Enter` 送出。
   背景的等待程序喚醒 session，把回答送到 agent。
4. **在任何一張卡片上留言。** 對卡片按 `i`。留言以同樣的方式送到 agent；agent 在卡片上回覆
   之前，留言標示為未回覆。
5. **讓守則標出沒有卡片就動工的 agent。** 修改檔案之前與每一輪結束時，hook 檢查以下這類
   守則：看板沒有泳道或卡片，agent 卻修改檔案；這一輪改了檔案，卻沒有任何卡片有 agent 的
   活動；還在討論的卡片在進行中的泳道；送達的回答沒有記下決定；完成的卡片沒有結果。每條
   守則可以在 `~/.config/kb/rules.toml` 設為關閉、警告或阻擋；`kb board check --history`
   列出過去的違反。
6. **選擇性地讓模型評測每一輪。** 守則看不到對話。提供 Gemini API 的金鑰後，背景的 hook
   在每一輪結束後請決策模型判斷，例如 agent 是否在回覆裡對某張卡片提出建議、卻沒有寫在卡片
   上，或是在對話裡等你回覆。發現顯示在即時訊息與卡片的標籤。沒有金鑰時不送出任何資料。
   有金鑰時，每一輪送出：你的訊息（最多 4,000 個字元）、送達的回答與留言、agent 使用的
   工具、它執行的看板指令、它最後的回覆，以及卡片的編號與標題。

TUI 只寫入兩種資料：回答與留言，而且是以只讀的資料庫角色，經由兩個資料庫函式寫入。看板上
其他的一切都由 agent 寫入。

## 運作方式

```
 Claude Code session                 本機
┌──────────────────────┐   kb hook <事件>    ┌──────────────┐
│ agent ── kb card ... ├────────────────────►│   kb（指令） │
│ hook：SessionStart、 │                     └──────┬───────┘
│ UserPromptSubmit、   │◄── 回答、留言、            │ writer 角色
│ PreToolUse、Stop ... │    守則的警告              ▼
└──────────────────────┘                     ┌──────────────┐
                                             │  PostgreSQL  │
                                             │  （Docker）  │
                                             └──────┬───────┘
                                                    │ reader 角色
                                                    ▼
                                             ┌──────────────┐
                                             │    kb tui    │ ◄── 你
                                             └──────────────┘
```

一個執行檔 `kb` 同時是 agent 的指令與你的畫面。agent 與 hook 以 writer 角色寫入；`kb tui`
以 reader 角色讀取。修改資料表需要另一個 admin 連線，它只取自環境變數 `KB_ADMIN_URL`，
不取自設定檔，避免 agent 無意間使用它。

## 需求與安裝

- Windows 11 與 PowerShell 7（`pwsh`），或 macOS（zsh 或 bash）。
- Docker Desktop 執行中。
- Rust 1.94 以上（`cargo`）。
- Claude Code。Windows 另外需要 Git Bash，Claude Code 用它執行 hook 的指令。

取得專案，並在專案資料夾執行安裝腳本：

```sh
git clone https://github.com/wustar576/tui-kanban.git
cd tui-kanban
pwsh scripts/setup.ps1     # Windows
bash scripts/setup.sh      # macOS（還沒有在實機上驗證）
```

腳本產生隨機的資料庫密碼寫入 `.env`，啟動 PostgreSQL 容器（`127.0.0.1:55432`），寫入
`~/.config/kb/config.toml`，以 `cargo build --release` 建置 `kb`，並套用資料表。它可以重複
執行，不改變既有的密碼與資料。接著把 `kb` 放進 `~/.cargo/bin`（rustup 已經把它加入 `PATH`）：
在專案資料夾執行 `cargo install --path crates/kb --locked`，或建立指向 `target/release/kb` 的
符號連結 `~/.cargo/bin/kb`。hook 直接執行 `kb`。寫在 `~/.zprofile` 的 `PATH` 只有登入 shell
會讀，以其他方式啟動的 session 的 hook 可能找不到 `kb`。

安裝 Claude Code 外掛。這個 repository 本身是名為 `tui-kanban` 的外掛 marketplace：

```sh
claude plugin marketplace add wustar576/tui-kanban
claude plugin install kb@tui-kanban
```

外掛只包含 hook 與技能 `/kb:kb`。它不安裝 `kb` 與資料庫；那是上面的安裝腳本的工作。先裝了
外掛時，agent 在 session 開始時得知 `kb` 不存在，`/kb:kb` 會提議安裝：你同意後，agent clone 這個 repository、執行
安裝腳本與 `cargo install --path crates/kb --locked`，然後開始看板。要改從本機的專案資料夾
安裝外掛時，執行 `claude plugin install kb --marketplace <專案資料夾>`。

要開啟模型評測時，在 Claude Code 裡輸入一次 `/plugin configure kb@tui-kanban` 並填入金鑰，
或把 `{"judge_api_key": "<金鑰>"}` 傳給 `claude plugin configure kb@tui-kanban --values-stdin`。
金鑰存在作業系統的憑證儲存區，不寫進檔案。

之後在 Claude Code 的 session 裡先與 agent 討論工作；要用看板時，輸入 `/kb:kb`（可以加上
標題，例如 `/kb:kb 修正登入逾時`）。在另一個終端機開啟 `kb tui`。

## 指令速查

| 指令 | 用途 |
| --- | --- |
| `kb tui` | 觀看最近變動的進行中看板 |
| `kb tui --board <session>` / `--workspace <名稱>` | 觀看指定的看板，或某個 workspace 最近的看板 |
| `kb tui --snapshot --width 100 --height 30` | 把看板畫面輸出成純文字後結束 |
| `kb board show [--all]` | 以文字輸出看板（`--json` 輸出 JSON） |
| `kb board list` / `kb workspace list` | 列出看板與 workspace |
| `kb card show <n>` | 輸出一張卡片的紀錄、提問與連結 |
| `kb activity` | 輸出活動軌跡 |
| `kb board check [--history]` | 立即檢查守則，或列出過去的違反 |
| `kb db status` | 檢查連線與資料表版本 |
| `kb --skill` | 輸出 agent 讀的完整用法 |

每個指令都接受 `--json`。指令的輸出是英文；卡片標題與泳道名稱照原樣顯示。

設計文件不在這個 repository 裡。程式碼註解裡引用的 `docs/...` 指的是那些文件。

## 狀態

早期版本。為作者自己使用 Claude Code 而做，不支援其他 agent。指令、資料表與 hook 的介面
都可能改變。

## 授權

MIT。見 [LICENSE](LICENSE)。
