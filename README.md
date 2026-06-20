# Open String

Rust製・単一バイナリで動作する、トークン効率と実行安全性を最優先に設計したPC操作AIエージェント。

> 詳細な要件定義は [`.docs/open-string-requirements.md`](.docs/open-string-requirements.md) を参照してください。本READMEはその実装状況に基づくユーザー向けの使い方ガイドです。

## 特徴

- **単一バイナリ**：追加ランタイム不要。リリースビルドはアイドル時メモリ消費 約11MB(目標50MB以下)。
- **Mediator / Sub Agent 分離アーキテクチャ**：ユーザーと対話する常駐の Mediator と、1タスクごとに使い捨てで生成される Sub Agent を分離し、ナレーション由来のトークン浪費を構造的に排除する。
- **段階的な実行権限管理**：`god mode` / `low security` / `middle permission` / `high protect` の4段階。危険操作（削除・送信・外部送信・課金・設定自己編集）は自動検出され、レベルに応じて確認を要求する。
- **コンテキスト管理に特化**：会話履歴の自動要約（Ctx Agent）、フェーズ境界検知、ツール結果クリアリング、外部進捗メモへの退避など、長時間セッションでのトークン肥大化を抑える仕組みを多重に備える。
- **Extension基盤（MCP準拠）**：公式バンドルの [`t0k3n-mcp`](https://github.com/tonrakun/t0k3n-mcp) をはじめ、任意の外部MCPサーバーやSKILLSを脱着可能な形で接続できる。設定変更はCore再起動なしでホットリロードされる。
- **TUI / GUI / チャットゲートウェイ**：ターミナルUI・ローカルWeb GUI・Discord/Telegram/LINE経由のいずれからも操作可能。チャットゲートウェイは既定で許可リスト制・確認操作は常に拒否・権限レベルは昇格不可にクランプされる。
- **認証情報は平文保存しない**：APIキー・Botトークンは常にOS標準のセキュアストレージ（Windows Credential Manager / macOS Keychain / Linux Secret Service）に保存される。

## 対応環境

| OS | ステータス |
|---|---|
| Windows | 実機検証済み（ファーストターゲット） |
| macOS | GitHub Actions CIでビルド・テスト確認済み |
| Linux | GitHub Actions CIでビルド・テスト確認済み |

## インストール

### リリースアーカイブから（推奨）

[Releases](../../releases) から環境に合ったアーカイブをダウンロードし、展開後に同梱のインストーラを実行してください。

```sh
# macOS / Linux
./install.sh
```

```powershell
# Windows
.\install.ps1
```

インストーラは実行ファイルをユーザーディレクトリ配下に配置し、PATHへの追加を行います（冪等）。実行中は各ステップ（バイナリ検出・ディレクトリ作成・コピー・PATH追加・バージョン確認）を番号付きで表示し、完了後にインストール先とバージョンを要約表示します。中身を確認したい場合は、各アーカイブとは別に [Releases](../../releases) からスクリプト単体（`install.ps1`/`install.sh`）も直接ダウンロードできます。

### ソースからビルド

```sh
git clone <this-repo>
cd open-string
cargo build --release
```

`rust-toolchain.toml` でツールチェーンが固定されているため、`rustup` が有効であれば追加設定なしでビルドできます。生成されるバイナリは `target/release/open-string`（Windowsは`.exe`）です。

## クイックスタート

```sh
# 1. Anthropic APIキーを登録（OSのセキュアストレージに保存される）
open-string auth login

# 2. ワークスペース（操作対象ディレクトリ）を作成・切り替え
open-string workspace create ./my-project
open-string workspace switch ./my-project

# 3. 対話を開始
open-string chat
```

初回はTUI/GUIのセットアップウィザードからでも同様の手順を実行できます。

```sh
open-string tui   # ターミナルUI
open-string gui   # ローカルWeb GUI（既定ブラウザで開く）
```

## コマンドリファレンス

主要なサブコマンドの一覧です。各コマンドの詳細は `open-string <command> --help` で確認できます。

| コマンド | 概要 |
|---|---|
| `auth login / status / logout` | APIキー認証の登録・確認・削除（`--workspace`でワークスペース別設定も可） |
| `permission status / set` | 実行権限レベルの確認・変更（`god-mode`有効化には`--confirm`が必要） |
| `workspace create / list / remove / switch / status` | ワークスペースの作成・一覧・削除・切り替え |
| `session list / end` | セッションの一覧・終了 |
| `extension list / add / remove / enable / disable / check / lifecycle / check-updates` | MCPサーバー/SKILLSの管理 |
| `agent run-task / run-tasks / prompt-versions / ctx-config` | Sub Agentへの単発・並列タスク実行、システムプロンプト断片の確認、Ctx Agent設定 |
| `chat` | Mediatorとの自然言語対話ループ（`--resume <session-id>`でセッション復元） |
| `health` | 起動時/任意実行のセルフヘルスチェック |
| `tui` / `gui` | ターミナルUI / ローカルWeb GUIの起動 |
| `gateway set-token / discord / telegram / line` | チャットゲートウェイのトークン登録とBot起動 |

## アーキテクチャ概要

```
ユーザー接点（TUI/GUI/チャットGW）
        │
        ▼
  Mediator Agent（仲介者・常駐）── 状態管理用にt0k3n-mcp等を自ら呼び出す
        │ タスク委譲（権限事前判定済みのみ）
        ▼
  Sub Agent（実行者・1タスク=1生成・使い捨て）── ナレーション禁止、作業ツール実行専従
        │
        ▼
  Extension（MCP準拠サーバー / SKILLS）
```

より詳細な設計判断・各項目の実装根拠は要件定義書の各セクション（特に4.2 コンテキスト管理、4.7 Mediator/Sub Agent分離）にチェックボックス単位で記録されています。

## セキュリティに関する注意

- すべての秘匿情報（APIキー、チャットゲートウェイのBotトークン/署名鍵）はOSのセキュアストレージにのみ保存され、平文ファイルには書き込まれません。
- `low security` / `middle permission` レベルでは、キーワードベースの危険操作検出に一致しない操作（任意のコマンド実行を含む）は確認なしで実行されます。厳密な制御が必要な場合は既定の `high protect` を使用してください。
- 依存関係は `cargo audit` で定期的に確認しています。既知の実害あるCVEはなく、`async-std`（テスト専用依存）・`paste`・`lru`（いずれも`ratatui`経由の間接依存）について保守終了/unsound警告が出ていますが、いずれも直接の実行パスには影響しません。

## 開発

```sh
cargo build --release   # ビルド
cargo test               # テスト（180件）
cargo clippy --all-targets -- -D warnings   # Lint
cargo fmt                # フォーマット
```

## ライセンス

[MIT License](LICENSE)
