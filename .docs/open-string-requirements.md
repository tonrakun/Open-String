# Open String 要件定義書（ドラフト v0.1）

> 糸・つながり・文字列。トークン効率と実行安全性を最優先に設計する、Rust単一バイナリ動作のPC操作AIエージェント。

---

## 1. プロジェクト概要

### 1.1 背景
- OpenClaw（旧Clawdbot/Moltbot）のようなPC操作AIエージェントが台頭しているが、Node.js実装中心でメモリ消費が大きく、設定ファイル（`~/.moltbot/`等）に認証情報が平文保存される等のセキュリティリスクが指摘されている。
- トークン消費の最適化（コンテキスト管理）に特化したツールとして開発者自身が `t0k3n-mcp`（Rust製MCPサーバー、最大87%のトークン削減）を開発済み。これをOpen Stringの公式Extensionとしてバンドルする。

### 1.2 目的
- Rust単一バイナリで動作する、軽量・低トークン消費に特化したPC操作AIエージェントを開発する。
- コンテキスト管理（システムプロンプト・会話履歴・セッション状態の圧縮）に最大のリソースを投下する。
- 実行権限を段階的に制御し、安全性とユーザー体験のバランスを取る。
- ユーザーとの対話を担う**Mediator Agent**と、実際のツール実行を担う使い捨ての**Sub Agent**を分離し、ナレーション由来のトークン浪費を構造的に排除する。

### 1.3 プロジェクト名の由来
Open String（オープン・ストリング）。「糸」「つながり」「文字列（トークン列）」を意味し、Core/Extensionが糸のように連結し合う構造、かつ最小限の文字列（トークン）でやり取りする思想を表す。

---

## 2. 用語定義

| 用語 | 定義 |
|---|---|
| Core | Open String本体。認証・権限・コンテキスト管理・UI・ゲートウェイ・セッション管理を担う中核部分 |
| Mediator Agent | ユーザーと自然言語で対話する唯一の常駐エージェント。権限事前判定・タスク分解・Sub Agent生成・結果集約・状態管理ツール呼び出しを担う |
| Sub Agent | 1タスクにつき1体生成される使い捨ての実行専従エージェント。自然言語ナレーション禁止、作業系ツール実行のみを行い、最小トークンで結果をMediatorに返す |
| Ctx Agent | Mediatorのコンテキストが肥大化（既定値：コンテキストウィンドウの70%）した際に一時生成される圧縮専用エージェント。会話履歴を要約しつつ全文をt0k3n-mcpのmemoryへ並行保存する |
| Extension | 外部MCPサーバー・SKILLS等、Coreの機能を拡張する仕組み全般 |
| t0k3n-mcp | 開発者制作の公式バンドルExtension。コード読み込みのトークン削減に特化 |
| ワークスペース | エージェントが作業対象とするプロジェクトディレクトリ単位の管理コンテキスト |
| セッション | 1回の対話継続単位。複数ワークスペースに跨ることも、1ワークスペース内で複数並行することもある |
| 権限レベル | god mode / low security / middle permission / high protect の4段階の実行許可ポリシー |
| ゲートウェイ | Discord/LINE/Telegram等チャットアプリとCoreを接続する処理系 |

---

## 3. 全体アーキテクチャ

```
┌───────────────────────────────────────────────────────────┐
│                      Open String Core                        │
│                                                               │
│  ユーザー接点（TUI/GUI/チャットGW）                              │
│         │                                                    │
│         ▼                                                    │
│  ┌─────────────────────────────────────────────────────┐   │
│  │            Mediator Agent（仲介者・常駐）                 │   │
│  │  - ユーザーと自然言語で対話する唯一の主体                    │   │
│  │  - OAuth認証 / 権限レベル事前判定 / タスク分解              │   │
│  │  - t0k3n-mcp等を「状態管理用途」で自ら呼び出す                │   │
│  │    （memory_save/get, session_snapshot/restore 等）       │   │
│  │  - 会話履歴・システムプロンプトのコンテキスト管理              │   │
│  │  - 複数Sub Agentの並列実行結果を集約                        │   │
│  └───────────────────────┬─────────────────────────────┘   │
│                           │ タスク委譲（許可済みのみ）            │
│                           ▼                                  │
│  ┌─────────────────────────────────────────────────────┐   │
│  │     Sub Agent（実行者・1タスク=1生成・使い捨て）             │   │
│  │  - 自然言語ナレーション一切禁止（実況・説明文の出力禁止）        │   │
│  │  - 作業系ツール実行に専従（検索・ファイル操作・コマンド実行等）   │   │
│  │  - t0k3n-mcp等を「作業効率化用途」で呼び出す                  │   │
│  │    （read_code_skeleton/body, batch_read 等）             │   │
│  │  - 権限チェックロジックは持たない（Mediatorが事前判定済み）     │   │
│  │  - Mediatorへの返却は最小トークン・高情報密度の構造化結果のみ    │   │
│  │  - 並列実行可（1タスクで複数Sub Agent同時実行→Mediator集約）  │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                               │
│  ┌───────────────────────────────────────────────────┐     │
│  │   セルフヘルスチェック / 自己修復層（Core共通基盤）        │     │
│  └───────────────────────────────────────────────────┘     │
└─────────────────────────┬─────────────────────────────────┘
                           │ Extension API（MCP準拠）
┌─────────────────────────┴─────────────────────────────────┐
│                  Open String Extension                       │
│  ┌────────────────┐  ┌─────────────────────┐               │
│  │公式: t0k3n-mcp   │  │任意の外部MCPサーバー  │               │
│  └────────────────┘  └─────────────────────┘               │
│  ┌────────────────┐                                        │
│  │SKILLS / その他拡張│                                        │
│  └────────────────┘                                        │
└───────────────────────────────────────────────────────────┘
```

### 3.1 設計原則
- Coreは単一バイナリで完結し、Extensionが一切無い状態でも最低限動作する。
- Extensionは脱着可能。t0k3n-mcpを含むあらゆるExtensionは「外部依存」として扱い、障害時はCoreが機能を縮退させてでも継続動作する。
- トークン削減はCoreとExtensionの二段構え：Core自体が会話履歴・システムプロンプトを最適化し、t0k3n-mcp等のExtensionがコード/ファイルアクセスを最適化する。
- **Core内部はMediator AgentとSub Agentに役割分離する（4.7参照）。** ユーザー対話・状態管理・権限判定はMediatorに集約し、実際のツール実行はすべて使い捨てのSub Agentに委譲することで、ナレーション由来のトークン浪費と権限ロジックの重複実装を排除する。

---

## 4. Core機能要件

### 4.1 OAuth認証・実行権限管理

> **方針変更（実装時の判断）**：当初想定の「Claude Code向けOAuth認証フロー」は、Anthropic公式が
> 提供するOAuth APIではなく、Claude Code CLI内部のリバースエンジニアリングされたPKCEフロー・
> 固定client_idを利用する方式であり、利用規約上のリスク（アカウント停止等）があるため採用しない。
> 代わりに**通常のAnthropic公式APIキー認証**を採用する。将来OAuthを含む他プロバイダを追加する
> 余地は認証プロバイダ抽象化層として残す。

- [x] Anthropic APIキー認証の実装（`AuthProvider`実装として`AnthropicApiKeyProvider`、`src/auth/api_key.rs`）
- [x] 認証プロバイダ抽象化層の設計（将来的に他プロバイダ・OAuth追加可能な構造。`AuthProvider`トレイト、`src/auth/mod.rs`）
- [x] 認証情報の暗号化ローカル保存（平文保存の禁止。`keyring` crate経由でOS標準セキュアストレージ（Windows Credential Manager / macOS Keychain / Linux Secret Service）に保存。6.3 311行目とも連携）
- [ ] トークンリフレッシュ処理（APIキー方式では対象外。将来OAuthプロバイダを追加する場合に再検討）
- [x] 認証セッションの失効・再認証フロー（`auth logout`での失効、`auth login`での再認証として実装）
- [x] 権限レベル4段階の定義と切り替えUI（`PermissionLevel`enum + `permission set`/`status`サブコマンド、`src/permission/`）
  - [x] `god mode`：全操作許可、確認なし。デフォルト無効。明示的な有効化操作必須（`--confirm`）、かつ起動毎の再確認を要求（`main()`起動時に毎回再確認プロンプト）
  - [x] `low security`：基本許可。削除・送信・課金・公開等の不可逆操作のみ確認を要求（レベル定義のみ。危険操作検出ロジック自体は116行目で別管理）
  - [x] `middle permission`：ディレクトリ・コマンドのホワイトリスト制。範囲外操作は確認を要求（レベル定義のみ。ホワイトリスト判定ロジックはMediator側で実装予定）
  - [x] `high protect`：原則すべての操作に確認を要求。読み取り専用操作のみ自動許可（デフォルト値として採用）
- [x] 権限レベルごとの操作ログ記録（`AuditLogger`/`FileAuditLogger`、`src/permission/audit_log.rs`。`permission set`・god mode再確認の判定結果を記録）
- [x] 危険操作（削除・送信・外部送信・課金）の検出ロジック（権限レベルに依らない共通フィルタ）（`classify`関数、`src/permission/danger.rs`）
- [x] **MCP設定ファイル等、Coreの動作に関わるコンフィグの自己編集も危険操作として権限管理の対象に含める**（5.4と連携。ユーザー確認を経た上での自動導入は許可するが、無断での設定変更は権限レベルに応じて拒否・確認要求する）（`DangerKind::ConfigEdit`/`CONFIG_EDIT_KEYWORDS`、`src/permission/danger.rs`）
- [x] ワークスペース単位での権限レベル個別設定（`WorkspacePermissionStore`、`src/permission/workspace_store.rs`。`--workspace`指定時はワークスペース配下`.open-string/permission`を優先し、未設定時はグローバル設定にフォールバック）
- [x] **権限チェックはMediator Agentが一元的に事前判定する**（4.7参照）。Sub Agent生成前にタスク内容と権限レベルを照合し、許可された範囲のタスクのみSub Agentへ委譲する（`Mediator::dispatch`/`dispatch_many`、`src/agent/mediator.rs`）
- [x] Sub Agent側には権限チェックロジックを実装しない（責務の単純化・軽量化。二重判定は行わない）（`SubAgent`は`permission`モジュールに一切依存しない、`src/agent/sub_agent.rs`）
- [x] Mediatorの事前判定をバイパスしてSub Agentが直接生成されることがないよう、Sub Agent生成経路をMediator経由に一本化する設計・実装（`SubAgent::new`は`pub(super)`で`Mediator`からのみ呼べる）

### 4.2 コンテキスト管理（最重要）

#### 4.2.1 システムプロンプトの動的構築
- [x] 固定巨大プロンプトを廃し、状況に応じた断片組み立て方式を採用（`SystemPromptBuilder`、`src/agent/system_prompt.rs`。narration-ban/permission/extension/read-onlyの各断片を組み合わせて生成し、固定文字列定数は廃止）
- [x] 現在の権限レベルに応じたプロンプト断片の切り替え（`permission_fragment(PermissionLevel)`が4段階それぞれの断片を返す）
- [x] 接続中Extension一覧に応じたツール説明の動的注入（未接続Extensionの説明は注入しない）（`SystemPromptBuilder::with_extensions`は渡されたExtensionのみ`## {name} usage`断片を追加し、未接続のものは一切言及しない）
- [x] t0k3n-mcp等、公式Extensionの「利用を促す指示（instructions）」をシステムプロンプトに標準組み込み（`ExtensionInfo::fragment`が各接続Extensionの利用指示を組み込む）
  - [x] Extension側がinstructionsファイル/フィールドを公開している場合、それを読み込んでプロンプトに反映する仕組み（`load_connected_extensions`が`extensions.json`マニフェストの`instructions_path`を読み込む）
  - [x] instructionsが存在しない場合のフォールバック（最小限の使用ガイドをCore側で自動生成）（`ExtensionInfo::fallback_instructions`）
- [x] プロンプトの圧縮済みテンプレートのバージョン管理（差分更新で再構築コストを抑える）（各`Fragment`が`(id, version)`を持ち、`SystemPromptBuilder::template_versions`で取得可能。CLI `agent prompt-versions`で確認可能）

#### 4.2.2 会話履歴・応答ログの管理（Core管轄、コード読み込みとは別軸）
- [x] チャット/TUI/GUI由来の発話ログの自動要約（古い履歴から段階的に圧縮）（`compact`、`src/agent/ctx_agent.rs`。`should_compact`の閾値到達で発火し、Ctx Agentが要約する）
- [x] フェーズ境界（タスク完了・モジュール完成等）の自動検知（`is_phase_boundary`：会話に矛盾(conflict)・拒否(denied)のない委譲バッチをチェックポイントとして検知）
- [x] フェーズ境界検知時の自動スナップショット保存→コンテキストクリア→リストアのフロー（`chat`、`src/main.rs`。フェーズ境界検知＋直近2*keep_recent_turns超で`compact`を即時発火し、`compact`内の`memory.save_history`(保存)→要約後historyへの置き換え(クリア/リストア)の既存フローを再利用）
- [x] 要約後も検索可能な索引（メタデータ）の保持（`MemoryStore::record_index_entry`/`FileMemoryStore`が`index.jsonl`に`{timestamp, label, summary}`を追記）
- [x] MCPツール応答・SKILL出力等、Extension由来の戻り値が肥大化した場合の自動圧縮・要約処理（ゲートウェイ層で吸収）（Sub Agent側`fetch_url`が肥大化レスポンスを切り詰め`src/agent/tools.rs`、Mediator側は`AggregatedReport`の圧縮済みサマリのみを履歴に記録するため生の戻り値はそもそも遡及しない）
- [x] **ツール実行ログ（生のリクエスト/レスポンス全文）はMediatorのコンテキストに原則含めない。** Sub Agentが返す結果サマリ（4.7.3）のみを会話履歴に記録し、生の実行過程は破棄またはオンデマンド再取得可能な外部ログとして退避する（`chat`の`history`には`Message::user_text`/`assistant_text`のみが積まれ、Sub Agent内のツール呼び出しループ(`ClaudeTaskExecutor::execute`)は完全に分離されたスコープで完結する）
- [x] ツール結果クリアリング：「呼び出しが行われたという記録」だけは履歴に残し、古い生の結果本体は破棄する軽量パターンの実装（要約処理より低コストな第一防衛線として、要約処理の手前に配置）（`clear_stale_tool_results`、`src/agent/ctx_agent.rs`。`should_compact`判定の前段で毎ターン実行し、`ToolResult`本体のみをマーカー文字列に置き換える）
- [x] 要約（Compaction）実行時、直近N件のやり取りは生のまま保持し、それより古い部分のみ要約対象とする（過剰要約による出力品質劣化・往復回数増加を防ぐため。Nは設定可能）（`CtxAgentConfig::keep_recent_turns`（既定4）。`compact`が`history`を`older`/`recent`に分割し、`recent`はそのまま結果に追加）
- [x] 過剰な要約による弊害（要約しすぎるとタスク完遂までの往復回数が増え、トータルのトークン削減効果が薄れる事例がある）を踏まえ、要約の発動閾値・粒度はベンチマークで調整可能な設計とする（`CtxAgentConfig`の`trigger_threshold_pct`/`target_size_pct`/`keep_recent_turns`はCLI `chat --ctx-trigger-threshold-pct`/`--ctx-target-size-pct`から調整可能）
- [x] 外部状態への退避（進捗ファイル/構造化メモ）：要約のロスを補うため、完了タスク・変更ファイル一覧・未解決事項等を構造化された外部メモ（例：`progress.md`相当）に書き出し、コンテキストリセット後に読み込み直す仕組み（`ProgressMemoStore`/`FileProgressMemoStore`、`src/agent/progress.rs`。完了タスクと拒否/競合（未解決事項）をMarkdownチェックリストとして`progress.md`へ追記し、`chat`起動時に読み込んで history へ復元する。変更ファイル一覧は専用の構造化フィールドが無く、各完了項目の`summary`本文に含まれる範囲でのベストエフォート）

#### 4.2.3 マルチセッション/マルチワークスペース状態管理
- [x] 複数ワークスペースを横断した状態管理レイヤー（`FileWorkspaceRegistry`/`FileSessionRegistry`、`src/session/`。グローバル設定ディレクトリのJSONファイルで全ワークスペースを横断管理し、current pointerで「現在のワークスペース」を保持）
- [x] ワークスペース単位のコンテキスト分離（メモリ・履歴・権限の独立性）（`session::memory_dir_for`/`progress_path_for`が`<workspace>/.open-string/{memory,progress.md}`を返し、`FileSessionRegistry::for_workspace`が`<workspace>/.open-string/sessions.json`を返す。既存の`WorkspacePermissionStore`と同じ`.open-string/`配置で権限・履歴・セッションがワークスペースごとに独立）
- [x] セッション一覧・現在状態のダッシュボード表示用データ提供（`open-string session list`が各セッションのid/label/開始時刻/active状態を構造化して出力。`SessionRegistry::list`がそのままTUI/GUIダッシュボード（4.3）からも呼び出せる）

#### 4.2.4 t0k3n-mcpとの責務分担（明確化）
- [ ] Core側はt0k3n-mcpがカバーする「ファイル/コード読み込みのトークン削減」には介入しない（重複実装を避ける）
- [ ] Core側が担うのは：システムプロンプト構築、会話履歴管理、マルチワークスペース管理、Extension応答の圧縮
- [ ] t0k3n-mcp不在時のフォールバック（簡易skeleton抽出ロジックをCore内に最低限保持するかは検討事項。4.2.5参照）

#### 4.2.5 外部Extensionのライフサイクル管理（新規要件）
- [x] 新規ワークスペース作成時に対応Extensionの自動セットアップを実行する仕組み（例：t0k3n-mcpの`setup`相当コマンドを自動実行）（`mcp::setup_workspace_extensions`、`src/mcp/lifecycle.rs`。`workspace create`実行直後に設定済みの全Extensionへ接続スモークテストを実行し結果を表示。MCPプロトコル自体に`setup`相当のRPCは存在しないため、接続可否の検証として実装）
- [x] 定期的なExtensionのバージョンチェック・自動アップグレード実行スケジューラ（例：t0k3n-mcpの`upgrade`相当コマンドを定期実行）（`mcp::check_for_updates`/`open-string extension check-updates`、`src/mcp/lifecycle.rs`。`initialize`ハンドシェイクの`serverInfo.version`を比較してバージョン変化を検知。常駐デーモンが存在しないため「定期」は`chat`起動時など既存の実行タイミングに相乗りする設計とし、本物のOSスケジューラ連携は未実装と明記。MCPに標準の`upgrade`RPCは存在しないため「自動アップグレード」は再接続→バージョン再検出に留まる）
- [x] アップグレード失敗時のロールバック機構（接続失敗時は`lastKnownVersion`/`lastCheckedAt`を更新せず直前の既知良好状態を保持。OSパッケージ管理レベルの実体的なダウングレードは行わない設計上の制約を明記）
- [x] Extensionバージョン不整合の検知とユーザーへの通知（`LifecycleOutcome::VersionChanged`を`extension check-updates`が表示）
- [x] Extension障害時のフェイルセーフ（Extension停止中でもCore本体機能は継続動作）（`connect_workspace_tools`/`connect_for_state_management`/ヘルスチェックいずれも接続失敗時はフェイルソフト。`src/agent/mcp_tools.rs`・`src/agent/mcp_memory.rs`・`src/health.rs`）
- [x] Extensionごとのライフサイクル設定（自動更新の有効/無効、更新頻度）をユーザーが上書き可能にする（`McpServerConfig::auto_update`/`update_check_interval_hours`、`open-string extension lifecycle`コマンド）

#### 4.2.6 コンテキスト隔離によるトークン削減（Mediator/Sub Agent分離、最重要）
- [x] ユーザー対話とツール実行を完全に分離する設計を採用する（詳細設計は4.7）。これはt0k3n-mcpが担うファイル/コード読み込み最適化とは独立した、別軸のトークン削減源として位置づける（Mediator(`src/agent/mediator.rs`)はSub Agent生成・結果集約のみを行い、ツール実行コードを一切持たない。ツール実行は`ClaudeTaskExecutor`/`SubAgent`に完全分離）
- [x] Sub Agentは使い捨てのクリーンなコンテキストで動作するため、長時間セッションでの履歴蓄積によるコンテキスト肥大化（Context Rot）がSub Agent側には発生しない構造とする（`ClaudeTaskExecutor::execute`はタスクごとに新規`messages`を生成し、呼び出し間で状態を持たない。1タスク=1回の実行で破棄される）
- [x] Mediator側のコンテキストには「タスクの委譲内容」と「Sub Agentからの圧縮済み結果」のみが蓄積され、ツール呼び出しの詳細過程は蓄積されない（`chat`の`history`（`src/main.rs`）には`Message::user_text`(依頼内容)と`Message::assistant_text`(自然言語応答)のみが積まれ、Sub Agent内部のツール呼び出しブロックは一切混入しない。4.2.2参照）

### 4.3 TUI / GUI

- [x] TUI：初期セットアップウィザード（`src/tui.rs`：APIキー入力→権限レベル選択→ワークスペース作成の3ステップ。`open-string tui`）
- [x] TUI：設定変更画面（権限レベル、Extension管理、認証管理）（`p`で権限レベル循環、`e`/`x`でExtension有効化・削除、`l`でログアウト。いずれも`dashboard::requires_confirmation`で危険操作判定）
- [x] TUI：ダッシュボード（セッション一覧、ワークスペース状態、トークン消費状況、ヘルスチェック結果）（`dashboard::gather`を共通データソースに使用）
- [x] GUI：初期セットアップウィザード（TUIと機能等価）（`src/gui.rs`+`src/gui/index.html`：ローカルHTTPサーバーを起動しブラウザでセットアップ画面を開く。`open-string gui`）
- [x] GUI：設定変更画面（TUIと機能等価）（同じ`dashboard`モジュール経由でTUIと同一の権限レベル/Extension/認証操作をWeb UIから実行）
- [x] GUI：ダッシュボード（TUIと機能等価、グラフィカルなトークン消費可視化を含む）（CSSバーによるトークン消費ゲージ、2秒ポーリングで自動更新）
- [x] TUI/GUI共通：操作ログのリアルタイム表示（`dashboard::gather`が`FileAuditLogger`の直近ログをtailし、TUI/GUI双方が定期再取得して表示）
- [x] TUI/GUI共通：危険操作確認ダイアログのレンダリング（`dashboard::requires_confirmation`+`PendingAction`/`apply_pending_action`を共通化。TUIはオーバーレイ、GUIはモーダルで同じ確認フローをレンダリング）

### 4.4 チャット連携ゲートウェイ

- [x] OpenClawのゲートウェイ実装を参照した設計方針の策定（OpenClaw自体のソースは参照不可のため、本要件定義書が明文化しているOpenClawの既知の問題点(「open設定での第三者操作」「~/.moltbot/の平文秘匿情報漏洩」「誤公開インスタンス」)を踏まえた設計方針を`src/gateway/mod.rs`冒頭のモジュールドキュメントとして明文化: 既定閉鎖の許可リスト、権限レベルのクランプ(エスカレーション禁止)、確認要求の自動拒否、トークンのOSキーリング保存、長文圧縮)
- [x] Rustによるゲートウェイ処理系の基盤実装（プロトコル非依存の抽象化層）（`src/gateway/mod.rs`の`ChatGateway`トレイト+`run`関数。Mediatorパイプラインへの接続・許可リスト判定・権限レベルクランプ・返信圧縮を全アダプタで共通化）
- [x] Discord連携アダプタ（`src/gateway/discord.rs`：Discord Gateway(WebSocket、`tungstenite`)でHELLO/IDENTIFY/ハートビート/MESSAGE_CREATEを処理し、REST APIで返信。実運用のBotトークンでの動作確認は未実施で、ドキュメント化されたプロトコルに基づく実装のみ）
- [x] LINE連携アダプタ（`src/gateway/line.rs`：`tiny_http`によるWebhook受信(`X-Line-Signature`のHMAC-SHA256検証込み)とPush APIによる返信。実チャネルでの動作確認は未実施）
- [x] Telegram連携アダプタ（`src/gateway/telegram.rs`：`getUpdates`ロングポーリングと`sendMessage`。実Botトークンでの動作確認は未実施)
- [x] チャット経由の指示に対する権限レベル適用（チャットからの操作は既定でより厳しい権限レベルを強制するか要検討）（`gateway::effective_level`が既定`high-protect`にクランプし、Core本体の設定がより緩くてもチャット経由では昇格させない。確認要求は`DeclineConfirmationPrompt`で常に拒否)
- [x] グループチャットでの誤操作防止（OpenClawで指摘された「open設定での第三者操作」リスクへの対策）（`GatewayConfig::allowed_senders`は既定で空(誰も許可しない)。`gateway <platform> --allow <id>`で明示的に許可された送信者のみMediatorに到達する)
- [x] チャット応答の長文圧縮（トークン消費を抑えた返信生成）（`gateway::compress_for_chat`が送信前に文字数上限で切り詰め、切り詰められたことを明示する)

### 4.5 セッション・ワークスペース管理

- [x] ワークスペースの作成・削除・切り替え（`open-string workspace create/list/remove/switch/status`、`FileWorkspaceRegistry`、`src/session/workspace.rs`。`switch`で設定したcurrentワークスペースは`--workspace`省略時のデフォルトとして`chat`/`agent`/`permission`系コマンドに反映される）
- [x] セッションの作成・一覧・終了（`open-string session list/end`、`FileSessionRegistry`、`src/session/registry.rs`。`chat`が起動時にセッションを開始し終了時に終了する）
- [ ] ワークスペースごとの設定（権限レベル、有効Extension、認証プロバイダ）の個別管理（権限レベルは`WorkspacePermissionStore`で個別管理済みだが、有効Extension・認証プロバイダのワークスペース別設定は未実装。5.1/5.3のExtension基盤実装時に対応予定）
- [x] セッション状態の永続化（snapshot/restore機構、4.2.2と連携）（`chat`が各ターン後に`FileMemoryStore::save_history`でセッション単位のスナップショットを保存し、`--resume <session-id>`で`FileMemoryStore::load_latest`から最新スナップショットを復元して会話を再開できる）

### 4.6 セルフヘルスチェック・自己修復（新規要件）

- [x] Core自身の起動時ヘルスチェック（バイナリ整合性、設定ファイル整合性、Extension接続状態）（`health::run_health_check`、`src/health.rs`。`chat`起動時に自動実行し、`open-string health`で単独実行も可能）
- [x] 定期ヘルスチェックのスケジューリング（常駐デーモンがないため、`chat`起動など既存の実行タイミングに相乗りする設計とした。OSスケジューラ（cron/Task Scheduler）連携による真の定期実行は未実装）
- [x] エラー検知時の自動分類（致命的/警告/情報）（`health::Severity::{Fatal,Warning,Info}`）
- [x] 自動リトライ機構（一時的なネットワーク/Extension接続エラー等）（Extension接続チェックは`EXTENSION_CONNECT_ATTEMPTS`回まで再試行、`src/health.rs`）
- [x] 自己修正ロジック（設定ファイルの破損検知時のデフォルト復元、依存Extensionの再インストール試行等）（`.mcp.json`破損時は破損ファイルを`.corrupt`にバックアップしデフォルト復元。Extensionの再インストール試行はOS依存のため対象外）
- [x] 自己修復不能と判断した場合のユーザーへの明示的エスカレーション（god mode等で無断修復させない設計）（修復不能時は`Severity::Fatal`としてユーザーへ表示しつつCoreは継続動作。god mode下でも無断修復はしない）
- [x] ヘルスチェック結果・自己修復履歴のダッシュボード表示（4.3と連携）（`open-string health`が`HealthReport`を表示。TUI/GUI連携自体は4.3実装時に対応）
- [x] 自己修復処理自体の権限レベル適用（自己修復もリスクのある操作のため、middle permission以上を要求等）（`health::can_self_repair`がhigh-protect未満の権限レベルでのみ自動修復を許可）

### 4.7 Mediator Agent / Sub Agent 分離アーキテクチャ（中核設計）

#### 4.7.1 Mediator Agent（仲介者・常駐）
- [x] ユーザー（チャット/TUI/GUI経由）と自然言語で対話する唯一の主体として実装する（CLI版の対話ループとして`open-string chat`を実装。`agent::plan`（`src/agent/conversation.rs`）が各ユーザー発話をClaudeへ送り、実行が必要なら`delegate_tasks`ツール呼び出しで`Task`群に分解、不要ならそのまま自然言語で直接応答。`main.rs`の`chat`関数がdirect応答とdelegated応答（`Mediator::dispatch_many_aggregated`→`natural_language_response`）を1ループで仲介し、ツール呼び出しの内部過程は履歴に残さずユーザー発話と最終応答のみを保持する。TUI/GUI版は4.3で別途実装）
- [x] Mediator自身は作業系ツール（検索・ファイル操作・コマンド実行等）を原則実行しない。実行が必要な場合は必ずSub Agentを生成して委譲する（`Mediator`構造体に作業系ツール実行コードは存在せず、`dispatch`/`dispatch_many`が唯一のSub Agent生成経路、`src/agent/mediator.rs`）
- [x] Mediatorはt0k3n-mcp等のExtensionを「状態管理用途」で自ら呼び出す（`memory_save/get`、`session_snapshot/restore`等）（`agent::connect_for_state_management`/`McpMemoryStore`、`src/agent/mcp_memory.rs`。`.mcp.json`で`memorySaveTool`/`memoryIndexTool`を宣言したExtensionが有効かつ権限互換なら、Ctx Agentの圧縮前バックアップ保存先としてローカル`FileMemoryStore`の代わりに使用。接続失敗時はローカルへフェイルソフト。`--resume`によるセッション復元は引き続きローカルの`FileMemoryStore::load_latest`のみを使用（Extension側の汎用的なget/restoreツール呼び出しは未実装）)
- [x] ユーザーからの依頼を受け、タスクを分解し、Sub Agentに渡すための専用システムプロンプト（スコープ・権限情報・利用可能ツール一覧）を生成する（`TaskScope::for_task`、`src/agent/scope.rs`。`Mediator::authorize`が確定した`PermissionLevel`とタスクの`read_only`から許可ツール一覧を算出し、`ClaudeTaskExecutor`はそれを`scope.describe()`としてシステムプロンプトに展開・ツール一覧をフィルタするのみで、ポリシー自体は決定しない。タスク分解＝ユーザー依頼の自然言語解釈は`agent::plan`（4.7.1の対話メインループ）が担い、CLI引数で個々のタスクを直接渡す`agent run-task(s)`系コマンドは引き続きスクリプト用途として残置）
- [x] 権限レベルに基づく事前判定を行い、許可されたタスクのみSub Agentへ委譲する（4.1と連携。Sub Agent側には権限ロジックを持たせない）（`Mediator::authorize`が`PermissionLevel::decide`で判定し、許可されない限りSubAgentは生成されない）
- [x] 複数Sub Agentを並列実行した場合、各Sub Agentからの結果を集約し、ユーザー向けの自然言語応答に変換する（`agent::natural_language_response`、`src/agent/respond.rs`。`AggregatedReport`をMediatorがClaudeClientへ直接渡し、自然言語の応答文に変換。API失敗時は構造化レポートの表示にフォールバック、`main.rs`の`print_structured_report`）
- [x] ユーザーとの対話履歴・進行中タスクの状態・ワークスペースごとのコンテキストを保持する（4.2.3と連携）（`chat`がセッション単位で会話履歴をスナップショット保存し`--resume`で復元、進行中タスクの未解決状態は4.2.2の進捗メモへ記録、ワークスペースごとのコンテキストは`session::memory_dir_for`/`progress_path_for`で分離。4.2.3参照）

#### 4.7.2 Sub Agent（実行者・1タスク=1生成・使い捨て）
- [x] 1タスクにつき1体のSub Agentを都度生成する（タスク完了後は破棄、状態を持ち越さない）（`SubAgent::run`は`self`を消費するため一度しか実行できない、`src/agent/sub_agent.rs`）
- [x] システムプロンプトにより、自然言語によるナレーション・実況・説明文の出力を明示的に禁止する（例：「Webを検索します」「ファイルを読み込んでいます」等の文言を一切出力しない）（`ClaudeTaskExecutor`の`SUB_AGENT_SYSTEM_PROMPT`で明示的に禁止、`src/agent/claude_executor.rs`）
- [x] Sub Agentの出力は、作業結果・成果物パス・状態変化・エラー情報等に限定する（`TaskResult { outcome, summary }`のみを返却、ナレーション用の出力経路は存在しない）
- [x] 作業系ツール（Web検索・ファイル操作・コマンド実行・外部MCP呼び出し等）の実行に専従する（ファイル操作・コマンド実行は`read_file`/`write_file`/`run_command`（`src/agent/tools.rs`）、基本的なWeb取得は`fetch_url`で実装済み。外部MCP呼び出しは`agent::connect_workspace_tools`+`ClaudeTaskExecutor::with_mcp_tools`（`src/agent/mcp_tools.rs`・`src/agent/claude_executor.rs`）で実装：`.mcp.json`の有効かつ権限互換なサーバーが広告するツールを`tools/list`で収集しClaudeのツール一覧へ追加、呼び出し時は`tools/call`で該当サーバーへルーティング。検索エンジン統合は専用の検索APIを直接組み込むのではなく、検索ツールを持つExtensionを接続すれば同じ汎用機構でSub Agentから利用可能になる設計とした）
- [x] t0k3n-mcp等のExtensionを「作業効率化用途」で呼び出す（`read_code_skeleton/body`、`batch_read`等）（上記と同じ汎用MCP呼び出し機構を使用。t0k3n-mcpを`.mcp.json`に登録すれば、advertiseされた`read_code_skeleton`/`read_code_body`/`batch_read`等のツールがSub Agentに自動的に提供される。t0k3n-mcp自体のデフォルトバンドル同梱は5.2/task 35で対応予定）
- [x] タスク管理・メモリ管理は一切行わない（これらはMediatorの責務。4.7.1参照）（`SubAgent`にタスク管理・メモリ管理コードは存在しない）
- [x] 権限チェックロジックを持たない（Mediatorが委譲前に判定済みのタスクのみを受け取る前提）（`src/agent/sub_agent.rs`は`permission`モジュールに一切依存しない）
- [x] 危険操作を実行しようとした場合でも、Mediatorが事前判定した権限スコープ外であれば実行不能な構成とする（ツールアクセス自体をスコープで制限）（`Mediator::authorize`が拒否した場合、`SubAgent`自体が生成されない）

#### 4.7.3 Mediator・Sub Agent間の結果受け渡し
- [x] Sub AgentからMediatorへの返却形式は固定スキーマに縛らず、「最小トークン数で最大の情報密度」を実現する可変設計とする（`TaskResult.summary`は自由形式の文字列、`src/agent/result.rs`）
- [x] Sub Agent自身が、Mediatorが次の判断を行うために必要十分な情報量まで結果を圧縮する責務を持つ（`TaskExecutor::execute`の戻り値は`TaskResult`のみで、生の実行過程を返す経路がない）
- [x] 冗長な実行過程（試行錯誤の中間結果、再取得可能な生データ）は返却対象から除外する（同上）
- [x] 将来的にMediator・Sub Agent間プロトコルとして軽量バイナリ/独自形式を検討する余地を残す（オープン課題として7章にも記載）（現状はプロセス内Rust型のみで、ワイヤー形式を固定していないため変更の余地を残している）

#### 4.7.4 並列実行
- [x] 1タスクに対して複数のSub Agentを同時生成し、並列実行することを許可する（`Mediator::dispatch_many`、`std::thread::scope`で実行、`src/agent/mediator.rs`）
- [x] 並列実行されたSub Agent群の結果はMediatorが集約し、矛盾や重複がある場合はMediatorが解決する（`Mediator::aggregate`/`dispatch_many_aggregated`、`src/agent/aggregate.rs`。同一description内で結果が完全一致するものは`AggregatedItem`に重複統合、不一致のものは`Conflict`として多数決（同数時はFailure優先）で解決しつつ全結果を保持）
- [x] 並列実行数の上限設定（リソース消費・API利用制限を踏まえた上限値、設定可能とする）（`MediatorConfig::max_parallel_sub_agents`、デフォルト4、`with_config`で変更可能）
- [x] 並列実行中の一部Sub Agentが失敗した場合のハンドリング（他のSub Agentの結果のみで進行するか、全体を再試行するかの方針）（方針：バッチ全体を中断せず、各タスクの結果を`Result`として個別に返す。`dispatch_many_continues_past_denied_and_failed_tasks`テストで確認）

#### 4.7.5 Ctx Agent（Mediatorのコンテキスト圧縮専用・一時生成）

Mediatorは常駐かつユーザーと長時間対話し続けるため、Sub Agent（使い捨て・短命）とは異なり、コンテキストが肥大化する唯一の対象となる。これに対処する専用の一時エージェントとしてCtx Agentを定義する。

- [x] **トリガー条件**：Mediatorの使用コンテキストが、使用モデルのコンテキストウィンドウの**70%（デフォルト値）**に到達した時点で発火する（`should_compact`、`src/agent/ctx_agent.rs`。文字数/4のラフなトークン推定値で判定。`main.rs`の`chat`ループが各ターン終了後に呼び出す）
- [x] トリガー閾値（70%）はユーザー設定で変更可能にする（デフォルト70%）（`CtxAgentConfig::trigger_threshold_pct`。CLIでは`open-string chat --ctx-trigger-threshold-pct`で上書き可能）
- [x] **発火タイミング**：閾値到達を検知しても即座に介入せず、Mediatorの現在進行中のターンが完了した時点でCtx Agentを生成する（応答中の文脈破壊を避ける）（`chat`関数内で`should_compact`の判定は`history`へのpush（ターン完了）の後にのみ実行、応答生成中には割り込まない）
- [x] **Ctx Agentの性質**：一時生成・使い捨て。圧縮処理完了後は終了し、状態を持ち越さない（`compact`はその場で実行される一回限りの関数呼び出しで、永続化された状態を持たない、`src/agent/ctx_agent.rs`）
- [x] **圧縮先サイズ**：使用モデルのコンテキストウィンドウの**10%（デフォルト値）**程度まで会話履歴を要約する。このしきい値もユーザー設定で変更可能とする（`CtxAgentConfig::target_size_pct`、デフォルト10。CLIでは`--ctx-target-size-pct`で上書き可能。`compact`が目標トークン数をシステムプロンプトに埋め込んでClaudeへ要約を依頼）
- [x] **並行保存処理**：Ctx Agentは要約処理と同時に、要約前のフル会話履歴をt0k3n-mcpの`memory`機能へ保存する処理を並行して実行する（要約と保存は同一トリガーで同時に走らせ、ロスレスな退避先を確保する）（`compact`が`std::thread::scope`で要約APIコールと`MemoryStore::save_history`を並行実行、`src/agent/ctx_agent.rs`。Coreにt0k3n-mcpを呼び出すMCPクライアントが未実装（4.2.4）のため、保存先は`MemoryStore`トレイトで抽象化し、現状はOS設定ディレクトリ配下にJSONを書き出す`FileMemoryStore`をデフォルト実装として使用。将来t0k3n-mcp連携が実装された時点で同トレイトを満たす実装に差し替え可能）
- [x] **要約への誘導文埋め込み**：生成する要約の末尾に、「さらに過去の履歴が必要な場合、またはユーザーから知らない事項を聞かれた場合は、t0k3nのmemoryを使用して詳細な履歴を取得すること」という固定の誘導指示を必ず含める（`CTX_AGENT_GUIDANCE_SUFFIX`を要約テキストの末尾に必ず連結、`src/agent/ctx_agent.rs`）
- [x] **差し替え処理**：Ctx Agentの圧縮完了後、Mediatorの会話履歴を要約版に差し替えてMediatorを再開する（Mediator自身の一時停止→再開のハンドオフ処理）（`main.rs`の`chat`ループが`compact`の戻り値で`history`変数を置き換え、次のユーザー入力からそのまま継続）
- [x] **適用範囲の明確化**：Ctx AgentはMediator専用の仕組みとする。Sub Agentは1タスク=1生成の使い捨てセッションであり、原理的にコンテキストが肥大化しない前提のため、Ctx Agent介入の対象外とする（`should_compact`/`compact`は`chat`関数内のMediator会話履歴にのみ適用され、`SubAgent`の実行経路（`dispatch`/`dispatch_many`）からは呼び出されない）
- [x] Ctx Agent自体の処理に失敗した場合のフェイルセーフ（要約失敗時にMediatorを強制終了させず、閾値到達前の状態を維持して再試行する等）（`compact`が`Err`を返した場合、`chat`は`history`を更新せず警告のみ出力して継続。次のターン終了後に閾値判定・再試行が自然に行われる、`src/main.rs`）

#### 4.8 初回インストール時
- [x] 初めてユーザーが本ソフトをインストールするときは環境に合わせたps1・shスクリプトを実行する（`scripts/install.ps1`（Windows）・`scripts/install.sh`（macOS/Linux）。リリースアーカイブ内で実行する前提のスタンドアロンスクリプト）
- [x] スクリプトはPATHの追加や必要なフォルダの作成等を自動で行う（インストール先ディレクトリ・Core設定ディレクトリを作成し、未登録の場合のみユーザーPATH（Windowsはレジストリ経由の永続PATH、Unixはshell rcファイル）に追記。冪等性を確認済み）
- [x] これらのスクリプト郡はGitHub Actionsによって作られたReleaseに同袍して公開される（`.github/workflows/release.yml`。`v*`タグ push時にWindows/macOS/Linux向けにビルドし、各バイナリと対応するインストールスクリプトをzip/tar.gzに同梱してGitHub Releaseへ添付）

---

## 5. Extension機能要件

### 5.1 Extension基盤
- [x] MCP準拠の外部サーバー接続インターフェース実装（`McpClient`、`src/mcp/client.rs`。stdio上のJSON-RPC 2.0でinitializeハンドシェイク・tools/list・tools/callを実装。I/Oを`Box<dyn Write/BufRead>`で抽象化し、実プロセスを起動せずプロトコル層を単体テスト可能にした）
- [x] SKILLS形式の拡張機能読み込み機構（`skills::load_skills`、`src/skills.rs`。`---`区切りのYAMLフロントマター（name/description）+本文を持つMarkdownファイルをワークスペースの`.open-string/skills/`から読み込み）
- [ ] Extension一覧管理・有効/無効切り替えUI（TUI/GUI連携）（CLI（`open-string extension list/enable/disable`）は実装済み。TUI/GUI側のUI連携は4.3実装時に対応）
- [x] Extensionごとの権限スコープ設定（Extensionが要求する権限とCoreの権限レベルの整合性チェック）（`McpServerConfig::required_permission_level`+`is_compatible_with`、`src/mcp/config.rs`。`open-string extension check`が接続前にCoreの現在権限レベルとの整合性を検証）

### 5.2 公式Extension: t0k3n-mcp バンドル
- [x] t0k3n-mcpをデフォルトバンドルとして同封（`agent::auto_register_t0k3n`、`src/agent/bundled_extensions.rs`・`src/mcp/bundled.rs`。実体のバイナリを同封するのではなく、`tonrakun/t0k3n-mcp`公式install.sh/install.ps1のインストール先（`~/.t0k3n-mcp/t0k3n`、Windowsは`%USERPROFILE%\t0k3n-mcp\t0k3n.exe`）またはPATH上に検出した場合のみ自動登録する設計。無断インストールはしない（5.4の「無断導入を防止」と同方針）)
- [x] `.mcp.json`相当の設定をCoreが自動生成（新規ワークスペース作成時、4.2.5と連携）（`workspace create`実行時に`mcp::default_server_config`で`--root`をワークスペースに固定したエントリを自動生成、未インストール時は何もしない）
- [x] t0k3n-mcpのinstructions/ドキュメントをCoreのプロンプト構築ロジックに自動連携（4.2.1と連携）（t0k3n-mcpはMCPの`initialize`で独自instructionsを公開しないため、Core側で用意した要約文を`.open-string/t0k3n-instructions.md`として書き出し、`agent::system_prompt::register_extension`で`extensions.json`に登録。4.2.1の`load_connected_extensions`がこれを読み込みシステムプロンプトに反映)
- [x] t0k3n-mcpのバージョン情報取得・表示（`McpClient::server_info`が`initialize`応答の`serverInfo.version`を取得し、`open-string extension check-updates`で表示。4.2.5と共通の汎用機構）
- [x] t0k3n-mcp無効化時の動作確認（Core単体での最低限動作保証）（`connect_workspace_tools`/`connect_for_state_management`/`health::run_health_check`はいずれも未接続・無効時にフェイルソフトし、Core本体機能（chat等）は継続動作することを既存テストで確認済み）

### 5.3 サードパーティExtension
- [x] 外部MCPサーバーの追加・削除UI（CLIの`extension add`/`extension remove`、および5.4のMediator経由の自然言語導入フローの双方から実行可能）
- [x] 互換性検証（Extension側のプロトコルバージョンチェック）（`McpClient`が`initialize`応答の`protocolVersion`を記録し、`is_protocol_compatible`でCore側の要求バージョンと比較。`health::run_health_check`は不一致をFatalではなくWarningとして扱い、Core本体の動作は継続。`extension check`/Mediator導入フローでも同じ判定を再利用）
- [x] サードパーティExtensionのサンドボックス化検討（権限レベルとの統合）（`McpToolSource.trusted`（bundled t0k3n以外はfalse）を導入し、`ClaudeTaskExecutor`がread-onlyタスクでは未信頼ツールを一切提示せず、それ以外のタスクでも呼び出し毎に`classify_danger`+`PermissionLevel::decide`を通し、`AutoAllow`以外は使い捨てSub Agentが確認を取れないため呼び出し自体を拒否）

### 5.4 Mediator主導によるExtension動的導入（新規要件）
- [x] ユーザーがMediatorに対し自然言語で「○○のMCPサーバーを使いたい」等を依頼した場合、Mediatorがその場でMCP設定（`.mcp.json`相当）を書き換えて導入できる仕組みを実装する（`propose_extension`ツールを介して`MediatorTurn::ProposeExtension`を返し、`chat`ループが`apply_proposed_extension`で`.mcp.json`に追記）
- [x] Mediatorによる設定ファイルの自己編集自体を「危険操作」の一種として権限レベル管理の対象に含める（4.1と連携。例：`middle permission`以上を要求等）（`permission::danger`の`ConfigEdit`分類を流用し、`PermissionLevel::decide`の`RequireConfirmation`判定に通す）
- [x] 導入対象のMCPサーバー情報（名称・接続先URL/コマンド・要求する権限スコープ）をユーザーに提示し、確認を得た上で設定変更を実行するフロー（無断導入を防止）（`ConfirmationPrompt::confirm`に名称・コマンド・引数・理由を含むサマリーを提示し、拒否時は`.mcp.json`を書き換えずに終了）
- [x] 導入後、5.5のホットリロード機構と連携し、Core再起動なしで即座に利用可能にする（`apply_proposed_extension`成功直後に`reload_chat_runtime`を呼び、同じ`chat`セッション内で`executor`/権限レベルを再構築）
- [x] 導入したMCPサーバーが信頼できないソース（未知の接続先等）である場合の警告表示（`untrusted_source_warning`：bundled t0k3n以外の名称は確認サマリーと結果メッセージの両方に警告を付与、`AutoAllow`経路でも表示）
- [x] 導入失敗時（接続不能・認証エラー等）のロールバック（設定ファイルを導入前の状態に復元）（`apply_proposed_extension`が追加直後に`McpClient::connect`で接続確認し、失敗時は`extension_remove`で`.mcp.json`を導入前の状態に戻す）

### 5.5 Extension/エージェント動作コンフィグのホットリロード（新規要件）
- [ ] MCPサーバー・SKILLSの追加・削除・設定変更をCore再起動なしで即時反映する仕組みを実装する（`.mcp.json`/Extension/権限レベルは`hotreload::ConfigWatcher`+`reload_chat_runtime`で対応済み。SKILLSは`chat`ループにまだ組み込まれておらず未対応 -- 別途SKILLS統合タスクで対応）
- [ ] Mediator/Sub Agent/Ctx Agentの動作に関わるコンフィグ（権限レベル設定、コンテキスト圧縮の閾値、システムプロンプト断片等）についても同様にホットリロード対応とする（権限レベルとExtension由来のシステムプロンプト断片は対応済み。コンテキスト圧縮閾値（`CtxAgentConfig`）はCLIオプションのみで永続設定ファイルが無く、ホットリロード対象として未対応）
- [x] ホットリロード発生時、実行中のSub Agent/Ctx Agentには影響を与えない（実行中タスクは旧設定のまま完走させ、次回生成以降から新設定を適用する）（`chat`のメインループの先頭、ターン境界でのみ`reload_chat_runtime`を呼ぶため、実行中のディスパッチには影響しない）
- [x] 設定ファイルの変更監視（ファイルシステムイベント検知）と、不正/破損した設定が読み込まれた場合のフォールバック（直前の正常な設定を保持して復元）（`hotreload::ConfigWatcher`が`notify`でファイル変更を検知。`reload_chat_runtime`は`mcp::load`/`store.load`が失敗した場合`None`を返し、呼び出し側は既存の`executor`/`permission_level`を保持したままフォールバック）
- [ ] ホットリロードの成功/失敗をTUI/GUIダッシュボードに通知（4.3と連携）（`FileHotReloadLog`に記録は実装済みだが、TUI/GUIダッシュボード自体（4.3）が未実装のため表示は未対応）
- [x] ホットリロード処理自体もセルフヘルスチェック層の監視対象に含める（4.6と連携）（`health::check_hot_reload`が`FileHotReloadLog`の直近結果を`HealthCheckItem`として`run_health_check`に追加）

---

## 6. 非機能要件

### 6.1 対応OS
- [ ] Windows対応（ファーストターゲット、優先実装）
- [ ] macOS対応（セカンドターゲット）
- [ ] Linux対応（セカンドターゲット）
- [x] クロスコンパイル/CI構成（単一バイナリビルドをOSごとに自動生成）

### 6.2 性能・リソース
- [ ] 単一バイナリでの配布（追加ランタイム不要）
- [ ] アイドル時メモリ消費量の目標値設定（要ベンチマーク、OpenClaw＝Node.js実装との比較を指標にする）
- [ ] トークン消費削減率のベンチマーク方針策定（t0k3n-mcp適用時/非適用時の比較）

### 6.3 セキュリティ
- [x] 認証情報の暗号化保存（OS標準のセキュアストレージ利用：Windows Credential Manager / macOS Keychain / Linux Secret Service。`keyring` crateで実装、4.1参照）
- [ ] 設定ファイルの平文秘匿情報禁止（OpenClawの`~/.moltbot/`漏洩事例の対策として明文化）
- [ ] 間接的プロンプトインジェクション対策（外部コンテンツ内の指示文言を実行指示として扱わないフィルタ層）
- [ ] チャットゲートウェイの公開設定デフォルトを「許可リスト制」とする（OpenClawの誤公開インスタンス問題への対策）
- [x] god mode利用時の追加警告・ログ強制記録（起動毎の再確認プロンプト＋`AuditLogger`による判定結果の強制記録、4.1参照）
