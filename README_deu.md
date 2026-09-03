<p align="center">
  <img src="images/HYDRA_UMC_BANNER.svg" alt="HYDRA-UMC-ORCHESTRATOR banner" width="100%">
</p>

# 🕸️ HYDRA-UMC-ORCHESTRATOR

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | 🇩🇪 <b>Deutsch</b> | <a href="README_zho.md">🇨🇳 简体中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a></p>

### 🤖 Verteilter Schwarm-Manager & Multi-Knoten-Koordinator

<p align="left">
  <img src="https://img.shields.io/badge/Lizenz-GPL%203.0-blue.svg" alt="GPL 3.0">
  <img src="https://img.shields.io/badge/Sprache-Rust%20%2F%20Go-orange.svg" alt="Rust/Go">
  <img src="https://img.shields.io/badge/Architektur-Distributed%20Edge-blue.svg" alt="Distributed">
  <img src="https://img.shields.io/badge/Sync-PTP%20%2F%20gRPC-yellow.svg" alt="Sync">
</p>

---

## 1. 🛠️ TECHNISCHER ÜBERBLICK

**HYDRA-UMC-ORCHESTRATOR** ist die übergeordnete Koordinationsschicht des HYDRA-UMC-Ökosystems. Er verwaltet mehrere HydraNodes (Kinematik-Brains, Vision-Knoten und kognitive Knoten) als einen einzigen, einheitlichen Schwarm.

Er übernimmt die globale Missionsplanung, den Lastausgleich innerhalb der Flotte und die Echtzeit-Synchronisation zwischen Robotern, um physische Kollisionen zu vermeiden und millimentergenaue Präzision bei kollaborativen Multi-Roboter-Aufgaben zu gewährleisten.

### Hauptmerkmale:
* 🕸️ **Schwarm-Koordination:** Orchestriert bis zu 32+ unabhängige Roboterarme über mehrere Controller.
* ⚖️ **Lastausgleich:** Weist Missionen automatisch dem am besten verfügbaren oder am besten ausgestatteten Roboter zu.
* 🛡️ **Zentralisierte Sicherheit:** Globales E-STOP-Management und flottenweite Zustandsüberwachung.
* 📡 **Einheitliche API:** Bietet einen einzigen Einstiegspunkt für Apps und Studios, um mit der gesamten Fabrik zu interagieren.
* 🧩 **Echtes v0 - Missions-Zustandsautomat:** `mission.rs` verfolgt jede Mission durch `Pending -> Dispatched -> InProgress -> Completed` (plus die Endzustände `Cancelled`/`Failed`), mit idempotenter Stornierung und echter Wiederherstellung, wenn ein Knoten als nicht erreichbar/ungültig gemeldet wird - siehe `mission-demo` unten. Reine In-Memory-Logik - kein live gRPC-Partner mit JOB-DISPATCHER/NODE-HEALING nötig, um sie auszuführen oder zu testen.

---

## 2. 🔄 ORCHESTRIERUNGSARCHITEKTUR

```mermaid
flowchart TB
    API["Externe API (Studios / Apps)"] --> ORCH["HYDRA-ORCHESTRATOR"]
    ORCH --> JOB["JOB-DISPATCHER (Missions-Warteschlange)"]
    JOB --> PATH["PATH-PLANNER-3D (Kollisionsprüfung)"]
    PATH --> SYNC["SWARM-SYNC (PTP-Synchronisation)"]
    SYNC --> NODE1["HydraNode 1 (H745)"]
    SYNC --> NODE2["HydraNode 2 (H745)"]
    ORCH --> HEAL["NODE-HEALING (Failover)"]
```

---

## 3. 🧠 ARCHITEKTUR UND ENTSCHEIDUNGEN

> Die folgenden internen Schichten sind das geplante Design für die Logik
> hinter diesem Einstiegspunkt. Unter „🔧 BUILD UND AUSFÜHRUNG“ steht, was heute
> tatsächlich läuft: ein echter, rein im Speicher lebender
> Missions-Zustandsautomat (`mission.rs`), noch ohne einen echten
> Netzwerkpartner, mit dem er sprechen könnte.

**Geplante interne Schichten**, die schrittweise auf dem bereits
existierenden echten Zustandsautomaten aufgebaut werden:
* **API-Schicht** — empfängt Missionsanfragen von Studios/Apps und übersetzt
  sie in Aktionen auf Flottenebene.
* **Missionswarteschlange** — übergibt angenommene Missionen an
  JOB-DISPATCHER und verfolgt ihren Lebenszyklus in der Flotte mit dem
  echten `Mission`/`MissionRegistry`-Zustandsautomaten, der bereits in
  `mission.rs` existiert - was noch fehlt, ist die gRPC-Verdrahtung zu
  einem echten JOB-DISPATCHER, an den Missionen übergeben werden.
* **PTP-synchronisierte Verteilung** — koordiniert die Zeitsteuerung mit
  SWARM-SYNC, damit mehrere Roboter derselben Mission gemäß den Prüfungen von
  PATH-PLANNER-3D kollisionsfrei bleiben.
* **Aggregierte Flottengesundheit** — fasst NODE-HEALING-Signale je Knoten in
  einer Gesamtansicht zusammen und ruft
  `MissionRegistry::recover_node_unavailable()` (bereits echt) auf, sobald
  ein Signal eintrifft; über diesen Weg würde auch ein globales E-STOP alle
  Knoten zugleich erreichen.

### Warum Rust für diesen speziellen Dienst

Dieser Prozess besitzt die größte Autorität über die Flotte: Er würde einen
globalen E-STOP auslösen und Missionen den Robotern zuordnen. Dafür sind
deterministische Koordination mit niedriger Latenz ohne GC-Pausen sowie
Speicher- und Typsicherheit zur Compile-Zeit nötig. Ein Absturz oder Data Race
bliebe nicht lokal, sondern könnte den gesamten Schwarm während einer Mission
ohne Koordinator lassen. JOB-DISPATCHER und NODE-HEALING verwenden Go, was für
ihre einfacheren, isolierten Aufgaben passt, nicht jedoch für diesen Kernprozess.

### Designentscheidungen

* **Einziger Prozess mit familienweitem `docker-compose.yml`.** Als
  Integrationsvater von SWARM-SYNC, PATH-PLANNER-3D, JOB-DISPATCHER und
  NODE-HEALING beschreibt dieses Repository die gemeinsame Ausführung;
  jedes Kind bleibt auf seine eigene Aufgabe fokussiert.
* **Stellt eine „Unified API“ für Apps/Studios bereit.** Clients sprechen
  diesen stabilen Einstiegspunkt an statt direkt vier Kinddienste; intern kann
  er auf die jeweilige Implementierung weiterleiten.
* **Warum `cancel()` idempotent ist, statt bei einer bereits stornierten Mission zu scheitern.** Eine Stornierungsanfrage kann legitim zweimal eintreffen - ein Client, der nach einer verlorenen Antwort erneut versucht, ein Bediener, der erneut auf Stornieren klickt, bevor er die Bestätigung sieht. Den zweiten Aufruf als Erfolg zu behandeln (`AlreadyCancelled`, kein Fehler) bedeutet, dass der Aufrufer nie unterscheiden muss zwischen „meine Stornierung hat funktioniert“ und „die Stornierung von jemand anderem hat bereits funktioniert“ - beides ist dasselbe gute Ergebnis. Das Stornieren einer `Completed`/`Failed`-Mission bleibt weiterhin verweigert: Das ist eine wirklich andere, nicht-idempotente Situation (das Rückgängigmachen abgeschlossener Arbeit), kein erneuter Versuch.
* **Warum die Wiederherstellung nach Knotenausfall zu `Pending` zurückstellt, statt die Mission sofort scheitern zu lassen.** Ein Knoten, der `UNREACHABLE` meldet, könnte gerade neu starten, statt dauerhaft verschwunden zu sein (siehe die eigene, begrenzte Retry-Logik von `HYDRA-UMC-NODE-HEALING`, die vorübergehende Störungen bereits abfängt, bevor ein Knoten überhaupt als ausgefallen gemeldet wird) - eine auf diesem Knoten hängende Mission bekommt daher über `Pending` eine neue Chance auf einem anderen Knoten, statt beim ersten Anzeichen von Problemen als `Failed` markiert zu werden. `fail()` existiert weiterhin für den Fall, dass die Neuverteilung wirklich keine Optionen mehr hat.

Für jedes reale mission-demo/CLI-Beispiel (aus einer echten kompilierten Binärdatei aufgezeichnet) siehe [`docs/CLI_REFERENCE.md`](docs/CLI_REFERENCE.md). Für den gemeinsamen `hydra.common.v1`-gRPC-Vertrag selbst - was er definiert, warum er in diesem Repo liegt und wie jede Sprache ihre eigenen Bindings generiert - siehe [`proto/README.md`](proto/README.md).

---

## 📂 VERZEICHNISSTRUKTUR

```text
HYDRA-UMC-ORCHESTRATOR/
├── src/
│   ├── mission.rs         # Echter Missions-Zustandsautomat (Mission, MissionRegistry)
│   ├── job_dispatcher.rs  # Echter Client für die eigene HTTP-API von HYDRA-UMC-JOB-DISPATCHER
│   ├── server.rs          # Einfache JSON/HTTP-Oberfläche (tiny_http, blockierend, ohne Async-Runtime)
│   └── main.rs            # Einstiegspunkt + echter Subbefehl `mission-demo`
├── proto/            # Gemeinsamer gRPC-Vertrag für Knoten-zu-Knoten-
│                     # Verkehr im gesamten Ökosystem (siehe
│                     # proto/README.md) - nicht nur die API dieses Repos
├── docs/             # Dokumentation und Architekturleitfäden
├── build/            # Kompilierte Binärdateien (Ausgabe von build.sh/.bat)
├── images/           # Medien und Diagramme
├── systemd/
│   └── hydra-umc-orchestrator.service # systemd-Unit der lokalen CM5-Mission-/Dispatch-API
├── tools/
│   ├── build_test.py # Nicht-versionierender Build-Check
│   └── ci_validate.py # Manifest/CHANGELOG/Docs-Validierung, von CI genutzt
├── Cargo.toml        # Rust-Paketmanifest (Name, Version, Abhängigkeiten)
├── bump_version.py   # Native Versions-Bump nach Kilometerzähler-Prinzip
├── bump_manifest_version.py # Synchronisiert die Version von hydra-umc.project.json mit der nativen (--sync)
├── build.sh/.bat     # Erhöht die Version, dann `cargo build --release`
├── build-test.sh/.bat # Nicht-versionierender Build-Check
├── run.sh/.bat       # Führt die kompilierte Binärdatei aus
├── docker-compose.yml # Integriert dieses Repo mit seinen 4 echten Kindern
└── README.md
```

Aus der ursprünglichen Vorlage entfernt: `hardware/`, `firmware/` und `os/`
— dies ist ein reiner Softwaredienst (Rust-Binärdatei) ohne eigene Hardware
oder Firmware und ohne zu pflegendes Betriebssystem-Image.

---

## 🔧 BUILD UND AUSFÜHRUNG

Der nackte Aufruf bleibt ein minimales Grundgerüst (gibt Identität aus,
beendet sich mit 0); der echte Missions-Zustandsautomat lässt sich schon
heute über `mission-demo` ausprobieren.

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

`build.sh`/`build.bat` erhöhen die Version in `Cargo.toml` (ökosystemweite
Kilometerzähler-Regel, siehe `bump_version.py`) und führen anschließend
`cargo build --release` aus. `run.sh`/`run.bat` führen die resultierende
Binärdatei direkt aus.

`mission-demo` führt ein echtes Szenario Ende-zu-Ende gegen den
`MissionRegistry` aus `mission.rs` aus und gibt jede echte Zustandsänderung
aus:

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
cargo test   # 42 Tests: jede Zustandsänderung, jede Ablehnung einer
             # ungültigen Zustandsänderung, idempotente Stornierung und
             # Wiederherstellung nach Knotenausfall
```

Als übergeordnetes Integrations-Repo des Ökosystems liefert dieses Repo auch
eine echte `docker-compose.yml`, die diesen Dienst zusammen mit seinen 4
Kindern (SWARM-SYNC, PATH-PLANNER-3D, JOB-DISPATCHER, NODE-HEALING) startet,
die als Geschwisterordner erwartet werden:

```bash
docker compose up --build
```

---

## 🚀 FAHRPLAN
* **Phase 1:** Deterministische Schwarm-Synchronisation über TSN und Sub-ms-Jitter-Reduzierung.
* **Phase 2:** 3D-Pfadplanung mit dynamischer Hindernisvermeidung in Multi-Roboter-Zellen.
* **Phase 3:** Multi-Roboter-Job-Dispatching-Optimierung unter Berücksichtigung der Ressourcenverfügbarkeit in Echtzeit.
* **Phase 4:** Implementierung eines Hochverfügbarkeits-Failover-Clusters und Unterstützung heterogener Roboter.

---

## 🔗 Verwandte Projekte

Dieses Projekt ist Teil des HYDRA-UMC-Robotik-Ökosystems desselben Autors (JuanenRac / Electro Hobby 3D). Gut zu wissen, da eine Anfrage eigentlich eines dieser Projekte betreffen könnte statt dieses Repositorys.

**Untergeordnete Projekte** — jedes davon ist ein Dienst, den dieser Orchestrator koordiniert oder direkt versorgt
- **[HYDRA-UMC-SWARM-SYNC](https://github.com/JuanenRac/HYDRA-UMC-SWARM-SYNC)** — echte CRDT-LWW-Element-Map-Zustandssynchronisation, eigenschaftsgetestet auf Multi-Zellen-Konvergenz.
- **[HYDRA-UMC-PATH-PLANNER-3D](https://github.com/JuanenRac/HYDRA-UMC-PATH-PLANNER-3D)** — echter RRT-basierter 3D-Pfadplaner mit echter Hindernis-/Arbeitsraum-Kollisionsvalidierung.
- **[HYDRA-UMC-JOB-DISPATCHER](https://github.com/JuanenRac/HYDRA-UMC-JOB-DISPATCHER)** — echte prioritätsbasierte Job-Queue mit Deduplizierung, über eine echte HTTP-API.
- **[HYDRA-UMC-NODE-HEALING](https://github.com/JuanenRac/HYDRA-UMC-NODE-HEALING)** — echter gRPC-basierter Flotten-Health-Watchdog mit Retry/Backoff und Identitäts-Mismatch-Erkennung.

**Direkt verwandt**
- **[HYDRA-UMC-SERVER](https://github.com/JuanenRac/HYDRA-UMC-SERVER)** — das reale Headless-Backend (REST/WebSocket), mit dem jeder Steuerungsclient tatsächlich spricht; dieser Orchestrator koordiniert mehrere Instanzen davon.
- **[HYDRA-UMC-COGNITIVE-NODE](https://github.com/JuanenRac/HYDRA-UMC-COGNITIVE-NODE)** — Integrationsknoten für die Hailo-10-Cognitive-Pipeline (LLM-/VLA-/Sprach-Orchestrierung); erhält von hier aus Missionsaufträge.
- **[HYDRA-UMC-SUITE](https://github.com/JuanenRac/HYDRA-UMC-SUITE)** — Desktop-Schwarmleitstand (PySide6) für mehrere Server gleichzeitig, verpackt als eigenständige ausführbare Datei; der Schwarmleitstand, den dieser Orchestrator absichert.

**Ebenfalls Teil des Ökosystems**

*Kern-Hardware & Plattform*
- **[HYDRA-UMC](https://github.com/JuanenRac/HYDRA-UMC)** — das physische Motherboard des Roboterarms: CM5-Host + Dual-Core-STM32H745, koordiniert bis zu 8 Werkzeugarme über CAN-OTA/SPI-OTA.
- **[HYDRA-UMC-OS](https://github.com/JuanenRac/HYDRA-UMC-OS)** — reproduzierbare Raspberry-Pi-OS-Produktschicht für den CM5: schreibgeschützter Agent, validierte Konfiguration/Profile, WiFi-Ersteinrichtung.
- **[HYDRA-UMC-SDK](https://github.com/JuanenRac/HYDRA-UMC-SDK)** — der gemeinsame JSON-Schema-Vertrag und die Sicherheitsschranke, gegen die jede Bridge ihre Befehle validiert.

*Kern-Backend & Clients*
- **[HYDRA-UMC-STUDIO](https://github.com/JuanenRac/HYDRA-UMC-STUDIO)** — Web-Steuerungs-Dashboard mit Echtzeit-3D-Visualisierung mehrerer Roboter.
- **[HYDRA-UMC-ANDROID-CONTROL](https://github.com/JuanenRac/HYDRA-UMC-ANDROID-CONTROL)** — native Android-Steuerungs-App mit biometrischem Login und einer gekoppelten Wear-OS-Begleit-App.
- **[HYDRA-UMC-IOS-CONTROL](https://github.com/JuanenRac/HYDRA-UMC-IOS-CONTROL)** — iOS/iPadOS-Steuerungs-App (Flutter) mit Echtzeit-WebSocket-Synchronisierung.
- **[HYDRA-UMC-DSI](https://github.com/JuanenRac/HYDRA-UMC-DSI)** — native Touch-UI für das eingebaute 7"-DSI-Touchscreen, direkt auf dem CM5 eingebettet.
- **[HYDRA-UMC-EDITOR-URDF](https://github.com/JuanenRac/HYDRA-UMC-EDITOR-URDF)** — grafischer Desktop-URDF-Ersteller/-Editor, der fertige Modelle in STUDIOs eigenen Katalog überträgt.
- **[HYDRA-UMC-BRIDGE-AMR](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-AMR)** — Koordinationsschranke für AGV-/AMR-Flotten über einen echten VDA-5050-MQTT-Publisher.
- **[HYDRA-UMC-BRIDGE-CNC](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-CNC)** — High-Level-Koordinator für CNC-Zellen mit echtem GRBL-Status-/Steuerbyte-Zugriff.
- **[HYDRA-UMC-BRIDGE-DROIDS](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-DROIDS)** — Koordinationsschranke für laufende/humanoide Droiden, mit einem echten Boston-Dynamics-Spot-Befehlssender.
- **[HYDRA-UMC-BRIDGE-LASER](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-LASER)** — Sicherheitskoordinator für Laserzellen, liest 3 echte Schlüssel-/Gehäuse-/Verriegelungs-GPIO-Sicherungen.
- **[HYDRA-UMC-BRIDGE-OPENPNP](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-OPENPNP)** — sicherer High-Level-Koordinator für den Leiterplattenfluss von OpenPnP Pick-and-Place.
- **[HYDRA-UMC-BRIDGE-PRINTER3D](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-PRINTER3D)** — sichere Koordinationsschranke für Moonraker/Klipper-3D-Drucker, mit echten gesicherten Job-Befehlen.
- **[HYDRA-UMC-BRIDGE-ROS2](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-ROS2)** — Sicherheitskoordinator mit einem echten, träge importierten rclpy-ROS-2-Transport.
- **[HYDRA-UMC-BRIDGE-UAV](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-UAV)** — Koordinationsschranke für kameraausgestattete UAVs, mit einem echten MAVLink-Befehlssender.

*URTC-Werkzeugplattform*
- **[URTC](https://github.com/JuanenRac/URTC)** — Firmware für die physische Universal-Robot-Tool-Controller-Platine, 25+ Werkzeugprofile über CAN-Bus.
- **[URTC-FLASHER](https://github.com/JuanenRac/URTC-FLASHER)** — Desktop-GUI-Flash-Tool für URTC-Platinen, CAN-OTA plus Full-Chip-SWD/JTAG.
- **[URTC-TESTER](https://github.com/JuanenRac/URTC-TESTER)** — Desktop-Live-CAN-Bus-Diagnosetool für URTC-Platinen, ein Panel pro Werkzeugprofil.
- **[URTC-WEB-STUDIO](https://github.com/JuanenRac/URTC-WEB-STUDIO)** — browserbasierte Alternative zu URTC-TESTER über die Web-Serial-API, ohne lokale Installation.

*Vision-KI-Knoten (Hailo-8)*
- **[HYDRA-UMC-VISION-NODE](https://github.com/JuanenRac/HYDRA-UMC-VISION-NODE)** — Integrationsknoten für die Hailo-8-Vision-Pipeline, mit einer echten stufenweisen Hardware-Bereitschaftsprüfung.
- **[HYDRA-UMC-DETECTION-HEF](https://github.com/JuanenRac/HYDRA-UMC-DETECTION-HEF)** — echte Registry für kompilierte Modelle mit Hailo-Architektur-/Prüfsummen-Safe-Load-Verifizierung.
- **[HYDRA-UMC-VISION-STREAMER](https://github.com/JuanenRac/HYDRA-UMC-VISION-STREAMER)** — echter GStreamer-Pipeline- + MediaMTX-Konfigurationsgenerator mit einer echten HailoRT-Integrationsschranke.
- **[HYDRA-UMC-VISUAL-SERVOING-API](https://github.com/JuanenRac/HYDRA-UMC-VISUAL-SERVOING-API)** — echtes Position-Based-Visual-Servoing-Korrekturgesetz, sicherheitsgesteuert nach vorgelagertem Zonenstatus.
- **[HYDRA-UMC-SAFETY-ZONES](https://github.com/JuanenRac/HYDRA-UMC-SAFETY-ZONES)** — echte Zonenverletzungsprüfung und E-STOP-Anforderung, mit erzwungener Kalibrierungsaktualität.

*Kognitiver KI-Knoten (Hailo-10)*
- **[HYDRA-UMC-VLA-ENGINE](https://github.com/JuanenRac/HYDRA-UMC-VLA-ENGINE)** — echte Aktions-Token-Kodierung/-Dekodierung und Trajektoriengenerierung für ein Vision-Language-Action-Modell.
- **[HYDRA-UMC-VOICE-UI](https://github.com/JuanenRac/HYDRA-UMC-VOICE-UI)** — echtes Sprach-Frontend (VAD + Intent-Parser) mit einem begrenzten, bestätigungsgesicherten Watch-Relay.
- **[HYDRA-UMC-SEMANTIC-PLANNER](https://github.com/JuanenRac/HYDRA-UMC-SEMANTIC-PLANNER)** — echte regelbasierte Aufgabenzerlegung und semantische Fehlerbehebung über MCU-Fehlercodes.
- **[HYDRA-UMC-DOCS-QA](https://github.com/JuanenRac/HYDRA-UMC-DOCS-QA)** — echte, nur auf der Standardbibliothek basierende TF-IDF-Dokumentensuche über die eigenen Markdown-Dokumente dieses Ökosystems.

*Digitaler Zwilling & Simulation*
- **[HYDRA-UMC-TWIN](https://github.com/JuanenRac/HYDRA-UMC-TWIN)** — Integrationsknoten für die Digital-Twin-Engine, mit einem echten Versionskompatibilitäts-Sync-Vertrag.
- **[HYDRA-UMC-HIL-BRIDGE](https://github.com/JuanenRac/HYDRA-UMC-HIL-BRIDGE)** — echte Hardware-in-the-Loop-Sicherheitsverriegelung, die Befehle zwischen Simulation und echter Hardware routet.
- **[HYDRA-UMC-PHYSICS-REPLICA](https://github.com/JuanenRac/HYDRA-UMC-PHYSICS-REPLICA)** — echte Vorwärtskinematik und Gelenkgrenzenvalidierung über eine echte URDF-Teilmenge.
- **[HYDRA-UMC-SYNTHETIC-DATA-GEN](https://github.com/JuanenRac/HYDRA-UMC-SYNTHETIC-DATA-GEN)** — echter prozeduraler 2D-Szenengenerator mit YOLO/COCO-Annotationsexport.

*Daten & Analytik*
- **[HYDRA-UMC-DATALAKE](https://github.com/JuanenRac/HYDRA-UMC-DATALAKE)** — echter sqlite3-gestützter Zeitreihenspeicher mit einer echten Ingest-/Abfrage-HTTP-API.
- **[HYDRA-UMC-ANOMALY-DETECTOR](https://github.com/JuanenRac/HYDRA-UMC-ANOMALY-DETECTOR)** — echter FFT- + statistischer Basislinien-Anomaliedetektor mit Drift-Überwachung.
- **[HYDRA-UMC-PRODUCTION-REPORTS](https://github.com/JuanenRac/HYDRA-UMC-PRODUCTION-REPORTS)** — echte OEE-/Verfügbarkeitsberechnung über den DATALAKE-Verlauf, mit reproduzierbarem CSV-Export.
- **[HYDRA-UMC-TELEMETRY-COLLECTOR](https://github.com/JuanenRac/HYDRA-UMC-TELEMETRY-COLLECTOR)** — echte CAN/WebSocket-Ingestion-Pipeline in DATALAKE, mit Sequenz-Deduplizierung.

*Industrie-Gateway*
- **[HYDRA-UMC-GATEWAY-INDUSTRIAL](https://github.com/JuanenRac/HYDRA-UMC-GATEWAY-INDUSTRIAL)** — Integrationsknoten, der zu Industrieprotokollen weiterleitet, mit einer echten Befehls-Allowlist-/Backpressure-Schicht.
- **[HYDRA-UMC-OPCUA-SERVER](https://github.com/JuanenRac/HYDRA-UMC-OPCUA-SERVER)** — echter OPC-UA-Adressraum, verifiziert mit einer echten Binärprotokoll-Client-Session.
- **[HYDRA-UMC-MQTT-BROKER](https://github.com/JuanenRac/HYDRA-UMC-MQTT-BROKER)** — echter MQTT-Broker mit optionaler Pro-Client-Authentifizierung und Topic-ACLs.
- **[HYDRA-UMC-MTCONNECT-ADAPTER](https://github.com/JuanenRac/HYDRA-UMC-MTCONNECT-ADAPTER)** — echte MTConnect-`/probe`- und `/current`-XML-Endpunkte mit Degraded-Mode-Ausgabe.

*Ergänzende Tools & Ökosystembetrieb*
- **[HYDRA-UMC-DASHBOARD-AI](https://github.com/JuanenRac/HYDRA-UMC-DASHBOARD-AI)** — Smart-Summaries- und Anomaly-Highlighting-Panels über DATALAKE/ANOMALY-DETECTOR, mit einem ehrlichen statistischen Fallback.
- **[HYDRA-UMC-TOOL-CLI](https://github.com/JuanenRac/HYDRA-UMC-TOOL-CLI)** — Flotten-CLI mit einem echten, stabilen Exit-Code-Vertrag, ein echter Live-Client der eigenen API von HYDRA-UMC-SERVER.
- **[HYDRA-UMC-WATCH](https://github.com/JuanenRac/HYDRA-UMC-WATCH)** — WearOS-Begleit-App mit echten haptischen Alarmen und einem Sprach-Relay zum gekoppelten Telefon.
- **[URTC-SMART-RACK](https://github.com/JuanenRac/URTC-SMART-RACK)** — Firmware für ein Platinenmontagegestell mit echter Werkzeug-ID-Dekodierung und Smart-Idle-Vorheizlogik.
- **[URTC-VISION-TOOL](https://github.com/JuanenRac/URTC-VISION-TOOL)** — Firmware plus ein echter Python-Vision-Begleiter für einen Thermal-/RGB-Inspektionswerkzeugkopf.
- **[HYDRA-UMC-UPDATER](https://github.com/JuanenRac/HYDRA-UMC-UPDATER)** — administratives Desktop-Tool, das jedes Repository in diesem Ökosystem entdeckt, klont und aktualisiert.


---

## 📚 Dokumentation & Community

- **[CONTRIBUTING.md](CONTRIBUTING.md)** — Technologie-Stack und Coding-Richtlinien für einen Pull Request.
- **[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)** — die in dieser Community erwarteten Verhaltensstandards.
- **[SECURITY.md](SECURITY.md)** — wie man eine Schwachstelle meldet, und die echten Sicherheitsschwerpunkte dieses Projekts.
- **[SUPPORT.md](SUPPORT.md)** — wo man Fragen stellt und Fehler meldet.
- **[LICENSE.md](LICENSE.md)** — die eigene Lizenz dieses Projekts.

## 👤 AUTOR
**JuanenRac** (Electro Hobby 3D)
📧 electrohobby3d@gmail.com
📺 [youtube.com/@electrohobby3d](https://youtube.com/@electrohobby3d)

## 📜 LIZENZ
GPL-3.0 - Siehe LICENSE für Details.
