<p align="center">
  <img src="images/HYDRA_UMC_BANNER.svg" alt="HYDRA-UMC-ORCHESTRATOR banner" width="100%">
</p>

# 🕸️ HYDRA-UMC-ORCHESTRATOR

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 简体中文</a> | 🇯🇵 <b>日本語</b></p>

### 🤖 分散型スウォームマネージャー & マルチノードコーディネーター

<p align="left">
  <img src="https://img.shields.io/badge/Licencia-GPL%203.0-blue.svg" alt="GPL 3.0">
  <img src="https://img.shields.io/badge/Language-Rust%20%2F%20Go-orange.svg" alt="Rust/Go">
  <img src="https://img.shields.io/badge/Architecture-Distributed%20Edge-blue.svg" alt="Distributed">
  <img src="https://img.shields.io/badge/Sync-PTP%20%2F%20gRPC-yellow.svg" alt="Sync">
</p>

---

## 1. 🛠️ 技術概要

**HYDRA-UMC-ORCHESTRATOR** は、HYDRA-UMC エコシステムの高レベル調整層
です。複数の HydraNode（運動学ブレイン、ビジョンノード、認知ノード）を
単一の統合されたスウォームとして管理します。

グローバルなミッション計画、フリート全体での負荷分散、そして物理的な
衝突を防ぎマルチロボット協調タスクにおけるミリメートル単位の精度を確保
するためのロボット間のリアルタイム同期を処理します。

### 主な機能：
* 🕸️ **スウォーム調整：** 複数のコントローラーにまたがる最大 32 台以上の独立したロボットアームをオーケストレートします。
* ⚖️ **負荷分散：** 最も空いている、または最適な装備を持つロボットに自動的にミッションを割り当てます。
* 🛡️ **集中安全管理：** グローバル E-STOP 管理とフリート全体の健全性監視。
* 📡 **統一 API：** アプリと Studio が工場全体とやり取りするための単一のエントリポイントを提供します。
* 🧩 **実装済み v0 —— ミッション状態機械：** `mission.rs` が各ミッションを `Pending -> Dispatched -> InProgress -> Completed`（および終端状態の `Cancelled`/`Failed`）を通じて追跡し、冪等なキャンセルと、ノードが到達不能/無効と報告された場合の実際のリカバリーを備えています——下記の `mission-demo` を参照してください。純粋なインメモリロジックであり、実行やテストに JOB-DISPATCHER/NODE-HEALING とのライブ gRPC ピアは不要です。

---

## 2. 🔄 オーケストレーションアーキテクチャ

```mermaid
flowchart TB
    API["External API (Studios / Apps)"] --> ORCH["HYDRA-ORCHESTRATOR"]
    ORCH --> JOB["JOB-DISPATCHER (Mission Queue)"]
    JOB --> PATH["PATH-PLANNER-3D (Collision Check)"]
    PATH --> SYNC["SWARM-SYNC (PTP Synchronization)"]
    SYNC --> NODE1["HydraNode 1 (H745)"]
    SYNC --> NODE2["HydraNode 2 (H745)"]
    ORCH --> HEAL["NODE-HEALING (Failover)"]
```

---

## 3. 🧠 アーキテクチャと設計上の決定

> 以下の内部レイヤーは、このエントリポイントの背後に置かれる予定のロジック
> の計画設計です——今日実際に動くものについては、さらに下の「🔧 ビルドと
> 実行」を参照してください：実際の、純粋にインメモリで動作するミッション
> 状態機械（`mission.rs`）であり、まだ話し相手となるライブのネットワーク
> ピアはありません。

すでに存在する実際の状態機械の上に段階的に構築される**計画中の内部
レイヤー**：
* **API 層** — Studio/アプリから高レベルのミッションリクエストを受け取り、フリートレベルのアクションに変換します。
* **ミッションキュー統合** — 受理されたミッションを、`mission.rs` にすでに存在する実際の `Mission`/`MissionRegistry` 状態機械を使って JOB-DISPATCHER に引き渡し、フリート全体にわたってそのライフサイクルを追跡します——まだ欠けているのは、ミッションを引き渡す先となる実際の JOB-DISPATCHER への gRPC 配線です。
* **PTP 同期ディスパッチ** — SWARM-SYNC とタイミングを協調させ、同一ミッションを実行する複数のロボットが PATH-PLANNER-3D のチェックに従って衝突のない状態を維持します。
* **フリート健全性の集約** — NODE-HEALING の各ノードからの信号を単一のフリート全体のビューに統合し、各信号が到着するたびに（すでに実際に動作する）`MissionRegistry::recover_node_unavailable()` を呼び出します。これはまた、グローバル E-STOP がすべてのノードに一度に到達するために通る経路でもあります。

### このサービスに特に Rust を採用した理由
このプロセスはフリートに対して最も大きな権限を持つものです：グローバル
E-STOP を発行し、どのロボットがどのミッションを担当するかを裁定します。
その役割には決定論的で低遅延な調整が必要です——安全に関わる停止を遅らせ
かねないガベージコレクションの一時停止は許容できません——また、コンパイル
時のメモリ/型安全性も必要です。なぜなら、ここでのクラッシュやデータ競合は
局所的に留まらず、ミッション実行中にスウォーム全体をコーディネーター不在
の状態に陥れる可能性があるからです。本オーケストレーター自身の 2 つの
子プロジェクト（JOB-DISPATCHER、NODE-HEALING）は代わりに Go を使用して
おり、それらのよりシンプルで独立したタスクにはよく適合しています。それは
この特定の「ブレイン」プロセスが必要とするトレードオフではありません。

### 設計上の決定
* **ファミリー全体の `docker-compose.yml` を持つ唯一のプロセス。** SWARM-SYNC、PATH-PLANNER-3D、JOB-DISPATCHER、NODE-HEALING の統合親プロジェクトとして、本リポジトリはファミリー全体がどのように連携して動作するかを記述する自然な場所です——各子プロジェクト自身のリポジトリは自分自身に集中したままにできます。
* **アプリ/Studio 向けに「統一 API」を公開。** すべてのクライアント（モバイルアプリ、デスクトップ版 Studio）が 4 つの独立した子サービスと直接やり取りするのではなく、ここにある 1 つの安定したエントリポイントとやり取りします。このエントリポイントは、その下層で変化する子実装のいずれにも自由にルーティングできます。
* **`cancel()` が、すでにキャンセル済みのミッションでエラーにするのではなく冪等である理由。** キャンセルリクエストは正当に二度届くことがあります——応答が失われた後にクライアントが再試行する場合や、確認を見る前にオペレーターが再度キャンセルをクリックする場合です。2 回目の呼び出しを成功（`AlreadyCancelled`、エラーではない）として扱うことで、呼び出し元は「自分のキャンセルが効いた」のか「誰か他の人のキャンセルがすでに効いていた」のかを決して区別する必要がなくなります——どちらも同じ良い結果だからです。`Completed`/`Failed` からのキャンセルは引き続き拒否されます：これは本当に異なる、非冪等な状況(完了した作業を取り消すこと)であり、再試行ではありません。
* **ノード障害からのリカバリーがミッションを即座に失敗させるのではなく `Pending` に再キューする理由。** `UNREACHABLE` を報告するノードは、永久に失われたのではなく、再起動中である可能性があります(HYDRA-UMC-NODE-HEALING 自身の上限付きリトライロジックを参照してください。そもそもノードがダウンしていると報告される前に、一時的な不調をすでに吸収しています)——そのため、そのノードで止まっていたミッションは、最初のトラブルの兆候で `Failed` にマークされるのではなく、`Pending` を通じて別のノードで新たなチャンスを得ます。再ディスパッチが本当に選択肢を使い果たした場合のために、`fail()` は引き続き存在します。

---

## 📂 リポジトリ構成

```text
HYDRA-UMC-ORCHESTRATOR/
├── src/
│   ├── mission.rs    # 実際のミッション状態機械（Mission、MissionRegistry）
│   └── main.rs       # エントリポイント + 実際の `mission-demo` サブコマンド
├── proto/            # エコシステム全体のノード間通信のための共有 gRPC
│                     # コントラクト（proto/README.md を参照）——本リポ
│                     # ジトリ自身の API だけではありません
├── docs/             # ドキュメントとアーキテクチャガイド
├── build/            # コンパイル済みバイナリ（build.sh/build.bat の出力）
├── images/           # メディアと図表
├── scripts/          # ユーティリティスクリプト
├── tools/
│   ├── build_test.py # バージョンを増やさないビルドチェック
│   └── ci_validate.py # CI が使用するマニフェスト/CHANGELOG/ドキュメント検証
├── Cargo.toml        # Rust パッケージマニフェスト（名前、バージョン、依存関係）
├── bump_version.py   # オドメーター式バージョンインクリメント、build.sh/.bat が実行
├── build.sh/.bat     # バージョンを増加させ、その後 `cargo build --release` を実行
├── build-test.sh/.bat # バージョンを増やさないビルドチェック
├── run.sh/.bat       # コンパイル済みバイナリを実行
├── docker-compose.yml # 本リポジトリを実際の 4 つの子プロジェクトと統合
└── README.md
```

元のテンプレートから省略：`hardware/`、`firmware/`、`os/` —— これは純粋
なソフトウェアサービス（Rust バイナリ）であり、専用のハードウェアや
ファームウェアを持たず、維持すべきオペレーティングシステムイメージも
ありません。

---

## 🔧 ビルドと実行

素の呼び出しは引き続き最小限のスケルトンのままです(識別情報を表示し、
終了コード 0 で終了)。実際のミッション状態機械は、今日 `mission-demo`
経由ですでに試すことができます。

```bash
# Windows
build.bat
run.bat
run.bat mission-demo

# Linux / macOS
./build.sh
./run.sh
./run.sh mission-demo
```

`build.sh`/`build.bat` は `Cargo.toml` のバージョンを増加させ（エコ
システム全体で統一されたオドメーター規則、`bump_version.py` を参照）、
その後 `cargo build --release` を実行します。`run.sh`/`run.bat` は
生成されたバイナリを直接実行します。

`mission-demo` は `mission.rs` の `MissionRegistry` に対して実際の
シナリオをエンドツーエンドで実行し、実際の状態遷移をすべて表示します：

```text
[orchestrator] mission-1: dispatched -> InProgress(node=node-a)
[orchestrator] mission-2: dispatched -> Dispatched(node=node-a)
[orchestrator] mission-3: dispatched -> InProgress(node=node-b)
[orchestrator] node-a reported UNREACHABLE by NODE-HEALING - recovering its missions
[orchestrator] mission-1: requeued -> Pending
[orchestrator] mission-2: requeued -> Pending
[orchestrator] mission-3: unaffected (different node) -> InProgress(node=node-b)
[orchestrator] mission-2: cancel() -> Cancelled -> Cancelled
[orchestrator] mission-2: cancel() again (idempotent) -> AlreadyCancelled -> Cancelled
[orchestrator] mission-3: complete() -> Completed(node=node-b)
[orchestrator] mission-4: fail() -> Failed(no healthy node accepted redispatch after 3 attempts)
[orchestrator] final registry state:
  mission-1: Pending (terminal=false)
  mission-2: Cancelled (terminal=true)
  mission-3: Completed(node=node-b) (terminal=true)
  mission-4: Failed(no healthy node accepted redispatch after 3 attempts) (terminal=true)
```

```bash
cargo test   # 22 件のテスト：すべての状態遷移、すべての不正な遷移の
             # 拒否、冪等なキャンセル、ノード障害後のリカバリー
```

エコシステムの統合親プロジェクトとして、本リポジトリは実際の
`docker-compose.yml` も提供しており、これにより本プロジェクトをその
4 つの子プロジェクト（SWARM-SYNC、PATH-PLANNER-3D、JOB-DISPATCHER、
NODE-HEALING、兄弟フォルダとしてチェックアウト）とともにビルド・実行
できます：

```bash
docker compose up --build
```

---

## 🚀 ロードマップ
* **フェーズ 1：** TSN による決定論的スウォーム同期とサブミリ秒ジッタの低減。
* **フェーズ 2：** マルチロボットセルにおける動的障害物回避を伴う 3D パスプランニング。
* **フェーズ 3：** リアルタイムのリソース可用性を用いたマルチロボットジョブディスパッチの最適化。
* **フェーズ 4：** 高可用性フェイルオーバークラスターの実装と異種ロボットのサポート。

---

## 🔗 関連プロジェクト

本プロジェクトは、同一著者（JuanenRac / Electro Hobby 3D）による、
ファームウェア、制御ソフトウェア、AI ノード、フリート管理ツールにまたがる、
より大きなロボティクスエコシステムの一部です。ご要望が実際にはこれらの
プロジェクトのいずれかに関するものであり、本リポジトリのものではない
可能性もあるため、知っておく価値があります。

### プロジェクトファミリー

**親プロジェクト：** なし —— 本プロジェクト自体が オーケストレーションと群制御 ファミリーの統合親プロジェクトです。

**子プロジェクト：**
- **[HYDRA-UMC-SWARM-SYNC](https://github.com/JuanenRac/HYDRA-UMC-SWARM-SYNC)** — 本オーケストレーターが調整するセル間での CRDT ベースの状態調整。
- **[HYDRA-UMC-PATH-PLANNER-3D](https://github.com/JuanenRac/HYDRA-UMC-PATH-PLANNER-3D)** — 本オーケストレーターがジョブをディスパッチする際の基準となる衝突のないパスプランニング。
- **[HYDRA-UMC-JOB-DISPATCHER](https://github.com/JuanenRac/HYDRA-UMC-JOB-DISPATCHER)** — 本オーケストレーターが供給するジョブキュー/スケジューラー。
- **[HYDRA-UMC-NODE-HEALING](https://github.com/JuanenRac/HYDRA-UMC-NODE-HEALING)** — 本オーケストレーターが管理する応答しないノードを検知し迂回します。

### 直接関連（ファミリー外）

- **[HYDRA-UMC-SERVER](https://github.com/JuanenRac/HYDRA-UMC-SERVER)** — このバックエンドの複数インスタンスを調整します。
- **[HYDRA-UMC-COGNITIVE-NODE](https://github.com/JuanenRac/HYDRA-UMC-COGNITIVE-NODE)** — ここからミッションレベルの命令を受け取ります。
- **[HYDRA-UMC-SUITE](https://github.com/JuanenRac/HYDRA-UMC-SUITE)** — 本オーケストレーターが支える群制御コマンドセンター。

### エコシステムのその他のプロジェクト

**HYDRA-UMC プラットフォーム** — マルチロボット・マイクロファクトリーセル
- **[HYDRA-UMC](https://github.com/JuanenRac/HYDRA-UMC)** — 最大 8 台のロボットアームを統括する CM5 + STM32H745 マザーボード。
- **[HYDRA-UMC-SERVER](https://github.com/JuanenRac/HYDRA-UMC-SERVER)** — すべての制御クライアントが接続する Express/WebSocket バックエンド。
- **[HYDRA-UMC-STUDIO](https://github.com/JuanenRac/HYDRA-UMC-STUDIO)** — Web ベースの制御ダッシュボード、マルチロボット 3D 可視化。
- **[HYDRA-UMC-ANDROID-CONTROL](https://github.com/JuanenRac/HYDRA-UMC-ANDROID-CONTROL)** — Wi-Fi/Bluetooth 経由の Android 制御アプリ。
- **[HYDRA-UMC-IOS-CONTROL](https://github.com/JuanenRac/HYDRA-UMC-IOS-CONTROL)** — Flutter で構築された iOS/iPadOS 制御アプリ。
- **[HYDRA-UMC-SUITE](https://github.com/JuanenRac/HYDRA-UMC-SUITE)** — デスクトップ版群制御コマンドセンター（Python/PySide6）。
- **[HYDRA-UMC-EDITOR-URDF](https://github.com/JuanenRac/HYDRA-UMC-EDITOR-URDF)** — ロボットカタログ向けのデスクトップ版 URDF モデルエディター。
- **[HYDRA-UMC-DSI](https://github.com/JuanenRac/HYDRA-UMC-DSI)** — 機載 DSI タッチスクリーン用のネイティブタッチ UI。

**URTC プラットフォーム** — すべての HYDRA-UMC ロボットアームが搭載するツールヘッドコントローラー
- **[URTC](https://github.com/JuanenRac/URTC)** — CAN バスツールヘッドコントローラー、25 種類のツールプロファイル。
- **[URTC-FLASHER](https://github.com/JuanenRac/URTC-FLASHER)** — デスクトップ版 CAN-OTA + SWD/JTAG フラッシュツール。
- **[URTC-TESTER](https://github.com/JuanenRac/URTC-TESTER)** — デスクトップ版ライブ CAN バス診断ツール。
- **[URTC-WEB-STUDIO](https://github.com/JuanenRac/URTC-WEB-STUDIO)** — Web Serial API によるブラウザベースの代替版。

**🎥 ビジョン AI ノード（Hailo-8）**
- [HYDRA-UMC-VISION-NODE](https://github.com/JuanenRac/HYDRA-UMC-VISION-NODE)
- [HYDRA-UMC-VISION-STREAMER](https://github.com/JuanenRac/HYDRA-UMC-VISION-STREAMER)
- [HYDRA-UMC-DETECTION-HEF](https://github.com/JuanenRac/HYDRA-UMC-DETECTION-HEF)
- [HYDRA-UMC-SAFETY-ZONES](https://github.com/JuanenRac/HYDRA-UMC-SAFETY-ZONES)
- [HYDRA-UMC-VISUAL-SERVOING-API](https://github.com/JuanenRac/HYDRA-UMC-VISUAL-SERVOING-API)

**🧠 認知 AI ノード（Hailo-10）**
- [HYDRA-UMC-COGNITIVE-NODE](https://github.com/JuanenRac/HYDRA-UMC-COGNITIVE-NODE)
- [HYDRA-UMC-VLA-ENGINE](https://github.com/JuanenRac/HYDRA-UMC-VLA-ENGINE)
- [HYDRA-UMC-VOICE-UI](https://github.com/JuanenRac/HYDRA-UMC-VOICE-UI)
- [HYDRA-UMC-SEMANTIC-PLANNER](https://github.com/JuanenRac/HYDRA-UMC-SEMANTIC-PLANNER)
- [HYDRA-UMC-DOCS-QA](https://github.com/JuanenRac/HYDRA-UMC-DOCS-QA)

**🎮 デジタルツインとシミュレーション**
- [HYDRA-UMC-TWIN](https://github.com/JuanenRac/HYDRA-UMC-TWIN)
- [HYDRA-UMC-PHYSICS-REPLICA](https://github.com/JuanenRac/HYDRA-UMC-PHYSICS-REPLICA)
- [HYDRA-UMC-HIL-BRIDGE](https://github.com/JuanenRac/HYDRA-UMC-HIL-BRIDGE)
- [HYDRA-UMC-SYNTHETIC-DATA-GEN](https://github.com/JuanenRac/HYDRA-UMC-SYNTHETIC-DATA-GEN)

**📊 データと分析**
- [HYDRA-UMC-DATALAKE](https://github.com/JuanenRac/HYDRA-UMC-DATALAKE)
- [HYDRA-UMC-TELEMETRY-COLLECTOR](https://github.com/JuanenRac/HYDRA-UMC-TELEMETRY-COLLECTOR)
- [HYDRA-UMC-ANOMALY-DETECTOR](https://github.com/JuanenRac/HYDRA-UMC-ANOMALY-DETECTOR)
- [HYDRA-UMC-PRODUCTION-REPORTS](https://github.com/JuanenRac/HYDRA-UMC-PRODUCTION-REPORTS)

**🏭 産業用ゲートウェイ**
- [HYDRA-UMC-GATEWAY-INDUSTRIAL](https://github.com/JuanenRac/HYDRA-UMC-GATEWAY-INDUSTRIAL)
- [HYDRA-UMC-OPCUA-SERVER](https://github.com/JuanenRac/HYDRA-UMC-OPCUA-SERVER)
- [HYDRA-UMC-MQTT-BROKER](https://github.com/JuanenRac/HYDRA-UMC-MQTT-BROKER)
- [HYDRA-UMC-MTCONNECT-ADAPTER](https://github.com/JuanenRac/HYDRA-UMC-MTCONNECT-ADAPTER)

**🛠️ 補完ツール**
- [URTC-SMART-RACK](https://github.com/JuanenRac/URTC-SMART-RACK)
- [URTC-VISION-TOOL](https://github.com/JuanenRac/URTC-VISION-TOOL)
- [HYDRA-UMC-WATCH](https://github.com/JuanenRac/HYDRA-UMC-WATCH)
- [HYDRA-UMC-TOOL-CLI](https://github.com/JuanenRac/HYDRA-UMC-TOOL-CLI)
- [HYDRA-UMC-DASHBOARD-AI](https://github.com/JuanenRac/HYDRA-UMC-DASHBOARD-AI)


## 👤 作者
**JuanenRac**（Electro Hobby 3D）
📧 electrohobby3d@gmail.com

## 📜 ライセンス
GPL-3.0 —— 詳細は LICENSE を参照してください。
