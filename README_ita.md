<p align="center">
  <img src="images/HYDRA_UMC_BANNER.svg" alt="HYDRA-UMC-ORCHESTRATOR banner" width="100%">
</p>

# 🕸️ HYDRA-UMC-ORCHESTRATOR

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | 🇮🇹 <b>Italiano</b> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 简体中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a></p>

### 🤖 Gestore dello sciame distribuito e coordinatore multi-nodo

<p align="left">
  <img src="https://img.shields.io/badge/Licenza-GPL%203.0-blue.svg" alt="GPL 3.0">
  <img src="https://img.shields.io/badge/Linguaggio-Rust%20%2F%20Go-orange.svg" alt="Rust/Go">
  <img src="https://img.shields.io/badge/Architettura-Distributed%20Edge-blue.svg" alt="Distributed">
  <img src="https://img.shields.io/badge/Sincronizzazione-PTP%20%2F%20gRPC-yellow.svg" alt="Sync">
</p>

---

## 1. 🛠️ PANORAMICA TECNICA

**HYDRA-UMC-ORCHESTRATOR** è lo strato di coordinamento di alto livello dell'ecosistema HYDRA-UMC. Gestisce più HydraNode (Kinematic Brain, Vision Node e Cognitive Node) come un unico sciame unificato.

Gestisce la pianificazione globale della missione, il bilanciamento del carico tra la flotta e la sincronizzazione in tempo reale tra i robot per prevenire collisioni fisiche e garantire una precisione millimetrica nelle attività collaborative multi-robot.

### Caratteristiche principali:
* 🕸️ **Coordinamento dello sciame:** Orchestrano oltre 32 bracci robotici indipendenti attraverso più controller.
* ⚖️ **Bilanciamento del carico:** Assegna automaticamente le missioni al robot più disponibile o meglio equipaggiato.
* 🛡️ **Sicurezza centralizzata:** Gestione globale dell'E-STOP e monitoraggio dello stato di salute dell'intera flotta.
* 📡 **API unificata:** Fornisce un unico punto di ingresso per APP e Studio per interagire con l'intera fabbrica.
* 🧩 **Reale v0 - Macchina a stati della missione:** `mission.rs` segue ogni missione attraverso `Pending -> Dispatched -> InProgress -> Completed` (più gli stati terminali `Cancelled`/`Failed`), con cancellazione idempotente e vero recupero quando un nodo viene segnalato irraggiungibile/non valido - vedi `mission-demo` più sotto. Logica pura in memoria - non serve nessun peer gRPC live con JOB-DISPATCHER/NODE-HEALING per eseguirla o testarla.

---

## 2. 🔄 ARCHITETTURA DI ORCHESTRAZIONE

```mermaid
flowchart TB
    API["API esterne (Studio / App)"] --> ORCH["HYDRA-ORCHESTRATOR"]
    ORCH --> JOB["JOB-DISPATCHER (Coda missioni)"]
    JOB --> PATH["PATH-PLANNER-3D (Controllo collisioni)"]
    PATH --> SYNC["SWARM-SYNC (Sincronizzazione PTP)"]
    SYNC --> NODE1["HydraNode 1 (H745)"]
    SYNC --> NODE2["HydraNode 2 (H745)"]
    ORCH --> HEAL["NODE-HEALING (Failover)"]
```

---

## 3. 🧠 ARCHITETTURA E DECISIONI DI PROGETTAZIONE

> I livelli interni seguenti sono il progetto previsto per la logica dietro
> questo punto di ingresso. Per ciò che funziona oggi, vedere «🔧 BUILD & RUN»
> più sotto: una macchina a stati della missione reale e puramente in memoria
> (`mission.rs`), ancora senza alcun peer di rete live con cui parlare.

**Livelli interni pianificati**, da sviluppare progressivamente sulla
macchina a stati reale che già esiste:
* **Livello API** — riceve richieste di missione di alto livello da Studios e
  app e le traduce in azioni a livello di flotta.
* **Integrazione della coda missioni** — consegna le missioni accettate a
  JOB-DISPATCHER e ne traccia il ciclo di vita nell'intera flotta usando la
  macchina a stati reale `Mission`/`MissionRegistry` che già esiste in
  `mission.rs` - ciò che manca ancora è il collegamento gRPC verso un vero
  JOB-DISPATCHER a cui consegnare le missioni.
* **Dispatch sincronizzato PTP** — coordina la temporizzazione con
  SWARM-SYNC affinché più robot nella stessa missione restino senza collisioni
  secondo i controlli di PATH-PLANNER-3D.
* **Aggregazione della salute della flotta** — raccoglie i segnali per nodo di
  NODE-HEALING in un'unica vista e chiama
  `MissionRegistry::recover_node_unavailable()` (già reale) non appena arriva
  ogni segnale; è anche il percorso di un E-STOP globale diretto a tutti i
  nodi contemporaneamente.

### Perché Rust per questo servizio specifico

Questo processo ha la maggiore autorità sulla flotta: emetterebbe un E-STOP
globale e decide quale robot riceve ciascuna missione. Richiede coordinamento
deterministico a bassa latenza senza pause del garbage collector, oltre a
sicurezza di memoria e tipi in compilazione. Un crash o una data race potrebbe
lasciare l'intero sciame senza coordinatore durante una missione.
JOB-DISPATCHER e NODE-HEALING usano Go, adatto ai loro compiti più semplici e
isolati, ma non a questo processo centrale.

### Decisioni di progettazione

* **Unico processo con `docker-compose.yml` per l'intera famiglia.** Come
  padre d'integrazione di SWARM-SYNC, PATH-PLANNER-3D, JOB-DISPATCHER e
  NODE-HEALING, questo repository descrive l'esecuzione dell'intera famiglia;
  ogni repository figlio resta concentrato sul proprio compito.
* **Espone una «API unificata» per app e Studios.** I client parlano con questo
  punto d'ingresso stabile anziché con quattro servizi figli; internamente può
  instradare verso la relativa implementazione.
* **Perché `cancel()` è idempotente invece di fallire su una missione già cancellata.** Una richiesta di cancellazione può legittimamente arrivare due volte - un client che riprova dopo una risposta persa, un operatore che clicca di nuovo cancella prima di vedere la conferma. Trattare la seconda chiamata come un successo (`AlreadyCancelled`, non un errore) significa che chi chiama non deve mai distinguere "la mia cancellazione ha funzionato" da "la cancellazione di qualcun altro ha già funzionato" - entrambi sono lo stesso buon risultato. Cancellare una missione `Completed`/`Failed` resta rifiutato: questa è una situazione genuinamente diversa, non idempotente (annullare lavoro finito), non un nuovo tentativo.
* **Perché il recupero da guasto nodo rimette in coda a `Pending` invece di far fallire subito la missione.** Un nodo che segnala `UNREACHABLE` potrebbe essere in fase di riavvio piuttosto che scomparso definitivamente (vedi la stessa logica di retry limitati di HYDRA-UMC-NODE-HEALING, che assorbe già i disturbi transitori prima ancora di segnalare un nodo come caduto) - quindi una missione bloccata su quel nodo ottiene una nuova possibilità su un altro nodo tramite `Pending`, invece di essere segnata `Failed` al primo segnale di problema. `fail()` esiste comunque per quando il redispatch esaurisce davvero le opzioni.

---

## 📂 STRUTTURA DELLE CARTELLE

```text
HYDRA-UMC-ORCHESTRATOR/
├── src/
│   ├── mission.rs    # Macchina a stati della missione reale (Mission, MissionRegistry)
│   └── main.rs       # Entry point + sottocomando reale `mission-demo`
├── proto/            # Contratto gRPC condiviso per il traffico nodo-a-nodo
│                     # in tutto l'ecosistema (vedi proto/README.md) - non
│                     # solo l'API di questo repo
├── docs/             # Documentazione e guide all'architettura
├── build/            # Binari compilati (output di build.sh/build.bat)
├── images/           # Media e diagrammi
├── scripts/          # Script di utilità
├── tools/
│   ├── build_test.py # Controllo build senza versionamento
│   └── ci_validate.py # Validazione manifest/CHANGELOG/docs usata dalla CI
├── Cargo.toml        # Manifesto del pacchetto Rust (nome, versione, dep)
├── bump_version.py   # Bump di versione stile contachilometri
├── build.sh/.bat     # Aggiorna la versione, poi `cargo build --release`
├── build-test.sh/.bat # Controllo build senza versionamento
├── run.sh/.bat       # Esegue il binario compilato
├── docker-compose.yml # Integra questo repo con i suoi 4 figli reali
└── README.md
```

Rimossi dal template originale: `hardware/`, `firmware/` e `os/` — è un
servizio puramente software (binario Rust) senza hardware o firmware propri,
e senza un'immagine del sistema operativo da mantenere.

---

## 🔧 BUILD ED ESECUZIONE

L'invocazione nuda resta uno scheletro minimo (stampa l'identità, esce con
0); la vera macchina a stati della missione è esercitabile oggi tramite
`mission-demo`.

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

`build.sh`/`build.bat` aggiornano la versione in `Cargo.toml` (regola
contachilometri dell'ecosistema, vedi `bump_version.py`) e poi eseguono
`cargo build --release`. `run.sh`/`run.bat` eseguono direttamente il binario
risultante.

`mission-demo` esegue uno scenario reale end-to-end contro il
`MissionRegistry` di `mission.rs` e stampa ogni transizione reale:

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
cargo test   # 22 test: ogni transizione, ogni rifiuto di transizione
             # non valida, cancellazione idempotente e recupero dopo
             # guasto nodo
```

Come repo padre di integrazione dell'ecosistema, include anche un vero
`docker-compose.yml` che avvia questo servizio insieme ai suoi 4 figli
(SWARM-SYNC, PATH-PLANNER-3D, JOB-DISPATCHER, NODE-HEALING), attesi come
cartelle gemelle:

```bash
docker compose up --build
```

---

## 🚀 TABELLA DI MARCIA
* **Fase 1:** Sincronizzazione deterministica dello sciame su TSN e riduzione del jitter sub-ms.
* **Fase 2:** Pianificazione dei percorsi 3D con evitamento dinamico degli ostacoli in celle multi-robot.
* **Fase 3:** Ottimizzazione del dispacciamento dei lavori multi-robot utilizzando la disponibilità delle risorse in tempo reale.
* **Fase 4:** Implementazione di cluster failover ad alta disponibilità e supporto per robot eterogenei.

---

## 🔗 Progetti Correlati

Questo progetto fa parte di un ecosistema robotico più ampio dello stesso autore (JuanenRac / Electro Hobby 3D), che copre firmware, software di controllo, nodi IA e strumenti di flotta. Utile saperlo, perché una richiesta potrebbe in realtà riguardare uno di questi progetti anziché questo repository.

### Famiglia

**Genitore:** nessuno — questo progetto è esso stesso il genitore di integrazione della famiglia Orchestrazione e Sciame.

**Figli:**
- **[HYDRA-UMC-SWARM-SYNC](https://github.com/JuanenRac/HYDRA-UMC-SWARM-SYNC)** — riconciliazione dello stato basata su CRDT tra le celle coordinate da questo orchestratore.
- **[HYDRA-UMC-PATH-PLANNER-3D](https://github.com/JuanenRac/HYDRA-UMC-PATH-PLANNER-3D)** — pianificazione di percorsi senza collisioni contro cui questo orchestratore assegna i lavori.
- **[HYDRA-UMC-JOB-DISPATCHER](https://github.com/JuanenRac/HYDRA-UMC-JOB-DISPATCHER)** — la coda/pianificatore di lavori che alimenta questo orchestratore.
- **[HYDRA-UMC-NODE-HEALING](https://github.com/JuanenRac/HYDRA-UMC-NODE-HEALING)** — rileva ed evita un nodo non risponente gestito da questo orchestratore.

### Relazione Diretta (fuori dalla famiglia)

- **[HYDRA-UMC-SERVER](https://github.com/JuanenRac/HYDRA-UMC-SERVER)** — coordina più istanze di questo backend.
- **[HYDRA-UMC-COGNITIVE-NODE](https://github.com/JuanenRac/HYDRA-UMC-COGNITIVE-NODE)** — riceve ordini di missione da qui.
- **[HYDRA-UMC-SUITE](https://github.com/JuanenRac/HYDRA-UMC-SUITE)** — il centro di comando sciame che sostiene questo orchestratore.

### Resto dell'Ecosistema

**Piattaforma HYDRA-UMC** — la cella di micro-fabbrica multi-robot
- **[HYDRA-UMC](https://github.com/JuanenRac/HYDRA-UMC)** — la scheda madre CM5 + STM32H745 che orchestra fino a 8 bracci robotici.
- **[HYDRA-UMC-SERVER](https://github.com/JuanenRac/HYDRA-UMC-SERVER)** — il backend Express/WebSocket con cui parla ogni client di controllo.
- **[HYDRA-UMC-STUDIO](https://github.com/JuanenRac/HYDRA-UMC-STUDIO)** — dashboard di controllo web, visualizzazione 3D multi-robot.
- **[HYDRA-UMC-ANDROID-CONTROL](https://github.com/JuanenRac/HYDRA-UMC-ANDROID-CONTROL)** — app di controllo Android via Wi-Fi/Bluetooth.
- **[HYDRA-UMC-IOS-CONTROL](https://github.com/JuanenRac/HYDRA-UMC-IOS-CONTROL)** — app di controllo iOS/iPadOS costruita in Flutter.
- **[HYDRA-UMC-SUITE](https://github.com/JuanenRac/HYDRA-UMC-SUITE)** — centro di comando sciame desktop (Python/PySide6).
- **[HYDRA-UMC-EDITOR-URDF](https://github.com/JuanenRac/HYDRA-UMC-EDITOR-URDF)** — editor desktop di modelli URDF per il catalogo robot.
- **[HYDRA-UMC-DSI](https://github.com/JuanenRac/HYDRA-UMC-DSI)** — interfaccia touch nativa per lo schermo DSI a bordo.

**Piattaforma URTC** — il controller della testa utensile che ogni braccio HYDRA-UMC porta con sé
- **[URTC](https://github.com/JuanenRac/URTC)** — controller testa utensile su bus CAN, 25 profili utensile.
- **[URTC-FLASHER](https://github.com/JuanenRac/URTC-FLASHER)** — strumento desktop di flashing CAN-OTA + SWD/JTAG.
- **[URTC-TESTER](https://github.com/JuanenRac/URTC-TESTER)** — strumento desktop di diagnostica CAN live.
- **[URTC-WEB-STUDIO](https://github.com/JuanenRac/URTC-WEB-STUDIO)** — alternativa basata su browser via Web Serial API.

**🎥 Nodo di Visione IA (Hailo-8)**
- [HYDRA-UMC-VISION-NODE](https://github.com/JuanenRac/HYDRA-UMC-VISION-NODE)
- [HYDRA-UMC-VISION-STREAMER](https://github.com/JuanenRac/HYDRA-UMC-VISION-STREAMER)
- [HYDRA-UMC-DETECTION-HEF](https://github.com/JuanenRac/HYDRA-UMC-DETECTION-HEF)
- [HYDRA-UMC-SAFETY-ZONES](https://github.com/JuanenRac/HYDRA-UMC-SAFETY-ZONES)
- [HYDRA-UMC-VISUAL-SERVOING-API](https://github.com/JuanenRac/HYDRA-UMC-VISUAL-SERVOING-API)

**🧠 Nodo IA Cognitiva (Hailo-10)**
- [HYDRA-UMC-COGNITIVE-NODE](https://github.com/JuanenRac/HYDRA-UMC-COGNITIVE-NODE)
- [HYDRA-UMC-VLA-ENGINE](https://github.com/JuanenRac/HYDRA-UMC-VLA-ENGINE)
- [HYDRA-UMC-VOICE-UI](https://github.com/JuanenRac/HYDRA-UMC-VOICE-UI)
- [HYDRA-UMC-SEMANTIC-PLANNER](https://github.com/JuanenRac/HYDRA-UMC-SEMANTIC-PLANNER)
- [HYDRA-UMC-DOCS-QA](https://github.com/JuanenRac/HYDRA-UMC-DOCS-QA)

**🎮 Gemello Digitale e Simulazione**
- [HYDRA-UMC-TWIN](https://github.com/JuanenRac/HYDRA-UMC-TWIN)
- [HYDRA-UMC-PHYSICS-REPLICA](https://github.com/JuanenRac/HYDRA-UMC-PHYSICS-REPLICA)
- [HYDRA-UMC-HIL-BRIDGE](https://github.com/JuanenRac/HYDRA-UMC-HIL-BRIDGE)
- [HYDRA-UMC-SYNTHETIC-DATA-GEN](https://github.com/JuanenRac/HYDRA-UMC-SYNTHETIC-DATA-GEN)

**📊 Dati e Analisi**
- [HYDRA-UMC-DATALAKE](https://github.com/JuanenRac/HYDRA-UMC-DATALAKE)
- [HYDRA-UMC-TELEMETRY-COLLECTOR](https://github.com/JuanenRac/HYDRA-UMC-TELEMETRY-COLLECTOR)
- [HYDRA-UMC-ANOMALY-DETECTOR](https://github.com/JuanenRac/HYDRA-UMC-ANOMALY-DETECTOR)
- [HYDRA-UMC-PRODUCTION-REPORTS](https://github.com/JuanenRac/HYDRA-UMC-PRODUCTION-REPORTS)

**🏭 Gateway Industriale**
- [HYDRA-UMC-GATEWAY-INDUSTRIAL](https://github.com/JuanenRac/HYDRA-UMC-GATEWAY-INDUSTRIAL)
- [HYDRA-UMC-OPCUA-SERVER](https://github.com/JuanenRac/HYDRA-UMC-OPCUA-SERVER)
- [HYDRA-UMC-MQTT-BROKER](https://github.com/JuanenRac/HYDRA-UMC-MQTT-BROKER)
- [HYDRA-UMC-MTCONNECT-ADAPTER](https://github.com/JuanenRac/HYDRA-UMC-MTCONNECT-ADAPTER)

**🛠️ Strumenti Complementari**
- [URTC-SMART-RACK](https://github.com/JuanenRac/URTC-SMART-RACK)
- [URTC-VISION-TOOL](https://github.com/JuanenRac/URTC-VISION-TOOL)
- [HYDRA-UMC-WATCH](https://github.com/JuanenRac/HYDRA-UMC-WATCH)
- [HYDRA-UMC-TOOL-CLI](https://github.com/JuanenRac/HYDRA-UMC-TOOL-CLI)
- [HYDRA-UMC-DASHBOARD-AI](https://github.com/JuanenRac/HYDRA-UMC-DASHBOARD-AI)


## 👤 AUTORE
**JuanenRac** (Electro Hobby 3D)
📧 electrohobby3d@gmail.com

## 📜 LICENZA
GPL-3.0 - Vedere LICENSE per i dettagli.
