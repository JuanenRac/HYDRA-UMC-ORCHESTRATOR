<p align="center">
  <img src="images/HYDRA_UMC_BANNER.svg" alt="HYDRA-UMC-ORCHESTRATOR banner" width="100%">
</p>

# 🕸️ HYDRA-UMC-ORCHESTRATOR

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | 🇮🇹 <b>Italiano</b> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 简体中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### 🤖 Gestore dello sciame distribuito e coordinatore multi-nodo

<p align="center">
  <img src="https://img.shields.io/badge/Licenza-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Linguaggio-Rust-orange.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Architettura-Distributed%20Edge-blue.svg" alt="Distributed">
  <img src="https://img.shields.io/badge/Sincronizzazione-JSON%2FHTTP-yellow.svg" alt="Sync">
</p>

---

**Verifica di onestà - cosa funziona davvero oggi:** la macchina a stati delle missioni (`src/mission.rs`: `Mission`/`MissionRegistry`, ogni transizione `Pending -> Dispatched -> InProgress -> Completed`/`Cancelled`/`Failed`, cancellazione idempotente, recupero da guasto del nodo), la sua persistenza su disco (`MissionRegistry::load`/`persist`, `data/missions.json`), l'outbox duraturo delle chiusure remote in sospeso (`src/outbox.rs`), il client HTTP reale verso HYDRA-UMC-JOB-DISPATCHER (`src/job_dispatcher.rs`) e il server JSON/HTTP semplice che espone tutto questo (`src/server.rs`, `tiny_http`, senza runtime async) sono reali e testati (143 test, `cargo test`). Il sottocomando CLI `mission-demo` esegue uno scenario reale end-to-end contro quello stesso `MissionRegistry` - non un mock. Ciò che resta aspirazionale: i "livelli interni pianificati" della sezione 3 più sotto (livello API, dispatch sincronizzato via PTP, aggregazione della salute della flotta come servizio permanente) descrivono un'intenzione progettuale, non codice già esistente; non c'è alcun cablaggio gRPC da questo repo verso un JOB-DISPATCHER o NODE-HEALING reale in esecuzione, nessuna integrazione con PATH-PLANNER-3D/SWARM-SYNC, e nessuna flotta multi-nodo reale ha mai girato contro questo binario - `docker-compose.yml` compila e avvia i 3 servizi gemelli insieme, ma ciò non è mai stato verificato su hardware robotico reale. Vedi `CHANGELOG.md` per ciò che è stato consegnato finora esattamente, e la ROADMAP più sotto per ciò che resta aperto.

---

> **Stato di sicurezza (safety e security):** questo ecosistema non è un sistema di sicurezza certificato. Non esiste ancora un percorso E-STOP hardware, né limiti o interblocchi imposti dal firmware, né un comando di movimento verificato; le immagini firmware sono controllate con CRC32 e un HMAC-SHA256 a chiave condivisa, non con una firma asimmetrica; TLS e TLS reciproco sono opzionali e disattivati per impostazione predefinita; nessuna scheda propria è stata ancora assemblata o provata. Leggi la [base di sicurezza](https://github.com/JuanenRac/HYDRA-UMC-DOCS/blob/main/docs/SECURITY_BASELINE.md) e il [modello delle minacce](https://github.com/JuanenRac/HYDRA-UMC-DOCS/blob/main/docs/THREAT_MODEL.md) prima di fidarti di qualsiasi affermazione qui per la sicurezza di una persona.

## 📋 In breve

| | |
|---|---|
| Versione | `0.1.9` |
| Maturità | Funzionale |
| Test | 143 (`cargo test`) |
| Tecnologia | Rust / tiny_http / ureq / JSON/HTTP |
| Destinazione | Il Raspberry Pi CM5 di una cella HYDRA-UMC |
| Su hardware reale | Non ancora: per questa versione non risulta alcuna esecuzione su hardware reale |

## 🚦 Stato del progetto

Ogni riga è una parte di questo repository e il semaforo dice a che punto è oggi. È scritta in base a ciò che il repository può mostrare (test, compilazioni, esecuzioni), non a ciò che è previsto.

🟢 Fatto e verificato · 🟡 Funziona, con limiti noti · 🟠 Fatto a metà o non ancora verificato · 🔴 Non fatto o non funzionante

| Area | Stato | Cosa significa qui |
|---|---|---|
| Software | 🟡 | Funziona per il suo scopo principale; la verifica di onestà qui sopra elenca ciò che manca ancora |
| Test | 🟢 | 143 test automatici, eseguiti a ogni modifica |
| Hardware reale | 🔴 | Non ancora: per questa versione non risulta alcuna esecuzione su hardware reale |
| Documentazione | 🟢 | README in otto lingue, verificato rispetto al manifesto a ogni modifica |

## 1. 🛠️ PANORAMICA TECNICA

**HYDRA-UMC-ORCHESTRATOR** è lo strato di coordinamento di alto livello dell'ecosistema HYDRA-UMC. È pensato per gestire più HydraNode (Kinematic Brain, Vision Node e Cognitive Node) come un unico sciame unificato.

Previsto: pianificazione globale delle missioni, bilanciamento del carico tra la flotta e sincronizzazione in tempo reale tra i robot per prevenire collisioni fisiche. Oggi segue le missioni con una macchina a stati e le passa a JOB-DISPATCHER; non sincronizza i robot, non previene collisioni e non garantisce alcuna precisione.

### Caratteristiche principali:
* 🧭 **Un solo proprietario dell'assegnazione:** con `--job-dispatcher-url` configurato, JOB-DISPATCHER decide quale robot riceve una missione e questo orchestratore possiede la missione stessa (ciclo di vita, chiusura e recupero). `POST /missions/:id/dispatch` (un nodo scelto a mano) viene allora rifiutato con 409 e rimanda a `POST /missions/:id/auto-dispatch`; senza job-dispatcher il dispatch manuale funziona come prima. *(implementato)*
* 📜 **Un contratto scritto con il job-dispatcher, verificato con i due processi reali:** `docs/JOB_DISPATCHER_CONTRACT.md` dice chi possiede cosa (il dispatcher, l'assegnazione del lavoro ai robot; questo orchestratore, la missione: il suo ciclo di vita, la chiusura e il recupero) e che cosa promette ciascuna parte all'altra, passo per passo. `tools/verify_dispatcher_contract.py` avvia il vero dispatcher (Go, dal suo checkout fratello) e il vero orchestratore e vi fa passare delle missioni: una missione è un lavoro in attesa, un'assegnazione manuale viene rifiutata, il dispatcher sceglie il robot, completare e fallire chiudono il lavoro, e recuperare un nodo gli toglie il lavoro e blocca un rapporto tardivo del tentativo perso. Viene saltato (uscita 0) se manca Go, il dispatcher o l'orchestratore compilato. *(implementato, 23 verifiche)*
* 🕸️ **Coordinamento dello sciame:** Orchestrano oltre 32 bracci robotici indipendenti attraverso più controller. *(pianificato)*
* ⚖️ **Bilanciamento del carico:** Assegna automaticamente le missioni al robot più disponibile o meglio equipaggiato.
* 🛡️ **Sicurezza centralizzata:** Gestione globale dell'E-STOP e monitoraggio dello stato di salute dell'intera flotta. *(pianificato)*
* 📡 **API unificata:** Fornisce un unico punto di ingresso per APP e Studio per interagire con l'intera fabbrica. *(pianificato)*
* 🧩 **Reale v0 - Macchina a stati della missione:** `mission.rs` segue ogni missione attraverso `Pending -> Dispatched -> InProgress -> Completed` (più gli stati terminali `Cancelled`/`Failed`), con cancellazione idempotente e vero recupero quando un nodo viene segnalato irraggiungibile/non valido - vedi `mission-demo` più sotto. Logica pura in memoria - non serve nessun peer gRPC live con JOB-DISPATCHER/NODE-HEALING per eseguirla o testarla.
* 🧱 **Sicuro per impostazione predefinita, e un rapporto in ritardo non può sovrascrivere un passaggio di consegne (v0):** l'API ascolta su `127.0.0.1:8114` salvo diversa indicazione; un altro indirizzo richiede un token bearer (`--token` / `ORCHESTRATOR_TOKEN`; allora tutte le route tranne `GET /health` lo richiedono) o un `--allow-anonymous` esplicito, e i corpi di richiesta oltre 64 KB sono rifiutati con `413`. Ogni dispatch incrementa l'`attempt` della missione; uno `start`/`complete`/`fail` con un `attempt` scaduto (o con il nome di un nodo che non detiene più la missione) viene rifiutato con `409` senza toccare la missione, così un nodo dichiarato non disponibile che torna da una partizione di rete non può sovrascrivere il nodo a cui la missione è stata passata (`--require-attempt` rende il campo obbligatorio). Il registro delle missioni e la outbox vengono forzati su disco prima di sostituire il file precedente, l'unità systemd fornita ora dà al processo una directory di stato scrivibile (prima `ProtectSystem=strict` rendeva la directory `data` predefinita di sola lettura e nulla sopravviveva a un riavvio), e `docker-compose.yml` ora avvia servizi veri (comandi `serve`, controlli di salute, token obbligatori, immagini fissate, solo l'orchestratore pubblicato e solo sul loopback dell'host). *(implementato)*
* 🔭 **Esercizio leggibile (opzionale):** `GET /stats` mostra anche l'età della chiusura non confermata più vecchia (`oldestPendingCloseAgeSeconds`) e l'ultimo successo e l'ultimo errore delle chiamate a JOB-DISPATCHER; `--log-format json` scrive un oggetto JSON per riga di log; `--keep-finished N` dimentica le missioni concluse più vecchie la cui chiusura è stata confermata, così `missions.json` non cresce all'infinito; il file porta uno `schema_version` e uno scritto da una versione più recente viene rifiutato; una chiusura dice a JOB-DISPATCHER quale tentativo sta chiudendo. Tutto disattivato o invariato per impostazione predefinita. *(implementato)*

---

## 2. 🔄 ARCHITETTURA DI ORCHESTRAZIONE

```mermaid
flowchart TB
    API["API esterne (Studio / App)"] --> ORCH["HYDRA-ORCHESTRATOR"]
    ORCH <--> JOB["JOB-DISPATCHER (Coda missioni)"]
    JOB -.-> PATH["PATH-PLANNER-3D (Controllo collisioni)"]
    PATH -.-> SYNC["SWARM-SYNC (Sincronizzazione PTP)"]
    SYNC -.-> NODE1["HydraNode 1 (H745)"]
    SYNC -.-> NODE2["HydraNode 2 (H745)"]
    HEAL["NODE-HEALING (Failover)"] --> ORCH
```

> Le frecce continue esistono oggi (l'API HTTP, JOB-DISPATCHER e NODE-HEALING che chiama `POST /nodes/:node/recover`). Le frecce tratteggiate sono pianificate, non implementate: nessun codice chiama PATH-PLANNER-3D o SWARM-SYNC.

---

## 3. 🧠 ARCHITETTURA E DECISIONI DI PROGETTAZIONE

> I livelli interni seguenti sono il progetto previsto per la logica dietro
> questo punto di ingresso. Per ciò che funziona oggi, vedere «🔧 BUILD ED ESECUZIONE»
> più sotto: una macchina a stati della missione reale e puramente in memoria
> (`mission.rs`), ancora senza alcun peer di rete live con cui parlare.

**Livelli interni pianificati**, da sviluppare progressivamente sulla
macchina a stati reale che già esiste:
* **Livello API** — riceve richieste di missione di alto livello da Studios e
  app e le traduce in azioni a livello di flotta.
* **Integrazione della coda missioni** — reale già ora, non solo
  pianificata: consegna le missioni accettate a JOB-DISPATCHER attraverso
  lo stesso trasporto reale HTTP/JSON che usa `server.rs` stesso
  (`src/job_dispatcher.rs`, i propri `submit_job`/`close_job`/
  `requeue_job`/`run_dispatch`, già collegati in ogni handler reale del
  ciclo di vita di una missione), e ne traccia il ciclo di vita nell'intera
  flotta usando la macchina a stati reale `Mission`/`MissionRegistry` che
  già esiste in `mission.rs`, duratura attraverso un vero riavvio del
  processo (`--data-dir`, `data/missions.json` di default - una missione ricaricata a metà volo diventa `Unknown` ed è rimessa in `Pending`, mai data per completata in silenzio). Questo livello
  non ha mai usato gRPC e non era mai stato pianificato che lo facesse -
  il contratto condiviso `HealthService` di `proto/hydra_common.proto` è
  una questione separata (la vera sonda di salute della flotta di
  HYDRA-UMC-NODE-HEALING stesso), non la consegna delle missioni a
  JOB-DISPATCHER di questo repository.
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
* **Perché il recupero da guasto nodo rimette in coda a `Pending` invece di far fallire subito la missione.** Un nodo che segnala `UNREACHABLE` potrebbe essere in fase di riavvio piuttosto che scomparso definitivamente (vedi la stessa logica di retry limitati di `HYDRA-UMC-NODE-HEALING`, che assorbe già i disturbi transitori prima ancora di segnalare un nodo come caduto) - quindi una missione bloccata su quel nodo ottiene una nuova possibilità su un altro nodo tramite `Pending`, invece di essere segnata `Failed` al primo segnale di problema. `fail()` esiste comunque per quando il redispatch esaurisce davvero le opzioni.

Per ogni esempio reale di mission-demo/CLI (catturato da un binario realmente compilato), vedere [`docs/CLI_REFERENCE.md`](docs/CLI_REFERENCE.md). Per il contratto gRPC condiviso `hydra.common.v1` stesso - cosa definisce, perché vive in questo repo e come ogni linguaggio genera i propri binding - vedere [`proto/README.md`](proto/README.md).

---

## 📂 STRUTTURA DELLE CARTELLE

```text
HYDRA-UMC-ORCHESTRATOR/
├── src/
│   ├── mission.rs         # Macchina a stati della missione reale (Mission, MissionRegistry)
│   ├── outbox.rs          # Casella in uscita reale e durevole delle chiusure remote in sospeso (sopravvive a un riavvio)
│   ├── job_dispatcher.rs  # Client reale per la API HTTP propria di HYDRA-UMC-JOB-DISPATCHER
│   ├── server.rs          # Superficie JSON/HTTP semplice (tiny_http, bloccante, senza runtime async)
│   ├── main.rs            # Entry point + sottocomando reale `mission-demo`
│   └── logging.rs         # Un unico posto per le righe che il processo scrive su stderr (testo semplice o il formato scelto con --log-format)
├── proto/            # Contratto gRPC condiviso per il traffico nodo-a-nodo
│                     # in tutto l'ecosistema (vedi proto/README.md) - non
│                     # solo l'API di questo repo
├── docs/             # Documentazione e guide all'architettura
├── build/            # Binari compilati (output di build.sh/build.bat)
├── images/           # Media e diagrammi
├── systemd/
│   └── hydra-umc-orchestrator.service # Unità systemd della API locale mission/dispatch sulla CM5
├── tools/
│   ├── build_test.py # Controllo build senza versionamento
│   ├── ci_validate.py # Validazione manifest/CHANGELOG/docs usata dalla CI
│   ├── verify_dispatcher_contract.py # Esegue il vero job-dispatcher e il vero orchestratore uno contro l'altro e verifica il contratto
│   ├── verify_compose.py # Controlli statici di docker-compose.yml (comandi, immagini fissate, porte su loopback, token obbligatori)
│   ├── _doc_policy.py    # Regole di documentazione condivise dalla famiglia (copia di quella di HYDRA-UMC-SDK, mantenuta sincronizzata)
│   ├── _readme_parity.py # Verifica che gli otto README abbiano la stessa struttura (copia di quella di HYDRA-UMC-SDK, mantenuta sincronizzata)
│   └── hydra_project_tool.py # Gli stessi comandi in ogni repository: validate, test, ci, version, status, bump
├── Cargo.toml        # Manifesto del pacchetto Rust (nome, versione, dep)
├── bump_version.py   # Incremento di versione stile contachilometri (eseguito solo dal comando `bump` esplicito)
├── bump_manifest_version.py # Sincronizza la versione di hydra-umc.project.json con quella nativa (--sync)
├── build.sh/.bat     # Aggiorna la versione, poi `cargo build --release`
├── build-test.sh/.bat # Controllo build senza versionamento
├── run.sh/.bat       # Esegue il binario compilato
├── docker-compose.yml # Integra questo repo con i suoi 3 figli reali
├── nodes.compose.json # Registro dei nodi che NODE-HEALING usa nello stack compose
├── .env.example # Token richiesti dallo stack compose (copiare in .env)
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

`build.sh`/`build.bat` non cambiano mai la versione (solo `python tools/hydra_project_tool.py bump` la alza) e poi eseguono
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
cargo test   # 143 test: le transizioni/i rifiuti di transizione non
             # valida/la cancellazione idempotente/il recupero dopo
             # guasto nodo di mission.rs, più la persistenza a prova di
             # crash di outbox.rs, il client HTTP reale di
             # job_dispatcher.rs e gli handler HTTP di server.rs
```

Come repo padre di integrazione dell'ecosistema, include anche un vero
`docker-compose.yml` che avvia questo servizio insieme ai suoi 3 figli
(SWARM-SYNC, JOB-DISPATCHER, NODE-HEALING; PATH-PLANNER-3D, uno strumento a riga di comando, resta dietro il profilo `tools`), attesi come
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

Questo progetto fa parte dell'ecosistema robotico HYDRA-UMC dello stesso autore (JuanenRac / Electro Hobby 3D). Vale la pena conoscerlo, poiché una richiesta potrebbe in realtà riguardare uno di questi invece di questo repository.

**Progetti Figli** — ciascuno è un servizio che questo orchestratore coordina o alimenta direttamente
- **[HYDRA-UMC-SWARM-SYNC](https://github.com/JuanenRac/HYDRA-UMC-SWARM-SYNC)** — vera sincronizzazione di stato CRDT LWW-Element-Map, con property test per la convergenza multi-cella.
- **[HYDRA-UMC-PATH-PLANNER-3D](https://github.com/JuanenRac/HYDRA-UMC-PATH-PLANNER-3D)** — vero pianificatore di percorsi 3D basato su RRT, con vera validazione delle collisioni ostacolo/spazio di lavoro.
- **[HYDRA-UMC-JOB-DISPATCHER](https://github.com/JuanenRac/HYDRA-UMC-JOB-DISPATCHER)** — vera coda di lavori basata su priorità con deduplicazione, su una vera API HTTP.
- **[HYDRA-UMC-NODE-HEALING](https://github.com/JuanenRac/HYDRA-UMC-NODE-HEALING)** — vero watchdog di salute della flotta basato su gRPC, con retry/backoff e rilevamento di discrepanza d'identità.

**Direttamente Correlati**
- **[HYDRA-UMC-SERVER](https://github.com/JuanenRac/HYDRA-UMC-SERVER)** — il vero backend headless (REST/WebSocket) con cui parla davvero ogni client di controllo; questo orchestratore coordina più istanze di esso.
- **[HYDRA-UMC-COGNITIVE-NODE](https://github.com/JuanenRac/HYDRA-UMC-COGNITIVE-NODE)** — hub di integrazione per la pipeline cognitiva Hailo-10 (orchestrazione LLM/VLA/voce); riceve ordini a livello di missione da qui.
- **[HYDRA-UMC-SUITE](https://github.com/JuanenRac/HYDRA-UMC-SUITE)** — centro di comando sciame desktop (PySide6) per più server contemporaneamente, pacchettizzato come eseguibile standalone; il centro di comando sciame supportato da questo orchestratore.

**Fa Anche Parte dell'Ecosistema**

*Hardware e Piattaforma di Base*
- **[HYDRA-UMC](https://github.com/JuanenRac/HYDRA-UMC)** — il progetto della scheda madre del braccio robotico: host CM5 + coprocessore STM32H745 dual-core, pensato per coordinare fino a 8 bracci utensile via CAN-OTA/SPI-OTA (firmware in fase iniziale; nessuna scheda assemblata per ora).
- **[HYDRA-UMC-OS](https://github.com/JuanenRac/HYDRA-UMC-OS)** — livello prodotto riproducibile su Raspberry Pi OS per il CM5: agente in sola lettura, config/profili validati, provisioning WiFi al primo contatto.
- **[HYDRA-UMC-SDK](https://github.com/JuanenRac/HYDRA-UMC-SDK)** — il contratto JSON-Schema condiviso e la barriera software di controllo dei job contro cui ogni bridge valida i propri comandi (non è una funzione di sicurezza certificata).
- **[HYDRA-UMC-CONNECTOR-HUB](https://github.com/JuanenRac/HYDRA-UMC-CONNECTOR-HUB)** — registro dichiarativo e validatore di manifesti adattatore per connettori di macchine esterne; estende la propria idea di contratto dell'SDK alle macchine esterne senza sostituire i progetti di gateway industriale.

*Backend Centrale e Client*
- **[HYDRA-UMC-STUDIO](https://github.com/JuanenRac/HYDRA-UMC-STUDIO)** — dashboard di controllo web con visualizzazione 3D multi-robot in tempo reale.
- **[HYDRA-UMC-ANDROID-CONTROL](https://github.com/JuanenRac/HYDRA-UMC-ANDROID-CONTROL)** — app di controllo nativa per Android con login biometrico e un companion Wear OS abbinato.
- **[HYDRA-UMC-IOS-CONTROL](https://github.com/JuanenRac/HYDRA-UMC-IOS-CONTROL)** — app di controllo per iOS/iPadOS (Flutter) con sincronizzazione WebSocket in tempo reale.
- **[HYDRA-UMC-DSI](https://github.com/JuanenRac/HYDRA-UMC-DSI)** — interfaccia touch nativa per il touchscreen DSI da 7" a bordo, incorporata direttamente nel CM5.
- **[HYDRA-UMC-EDITOR-URDF](https://github.com/JuanenRac/HYDRA-UMC-EDITOR-URDF)** — creatore/editor grafico desktop di URDF che invia i modelli finiti al catalogo di STUDIO.
- **[HYDRA-UMC-EDITOR-STL](https://github.com/JuanenRac/HYDRA-UMC-EDITOR-STL)** — editor desktop di modelli STL, che trasforma/sostituisce/rimuove/aggiunge veri pezzi nello stesso catalogo di modelli modificato anche da HYDRA-UMC-EDITOR-URDF.
- **[HYDRA-UMC-BRIDGE-AMR](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-AMR)** — barriera di coordinamento per flotte AGV/AMR tramite un publisher MQTT VDA 5050 (testato solo contro una flotta emulata).
- **[HYDRA-UMC-BRIDGE-CNC](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-CNC)** — coordinatore ad alto livello per celle CNC con accesso a stato/byte di controllo GRBL (testato solo contro un emulatore).
- **[HYDRA-UMC-BRIDGE-DROIDS](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-DROIDS)** — barriera di coordinamento per droidi con zampe/umanoidi, con un mittente di comandi per Boston Dynamics Spot (testato solo contro un emulatore).
- **[HYDRA-UMC-BRIDGE-LASER](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-LASER)** — coordinatore per celle laser che legge 3 segnali GPIO di chiave/involucro/interblocco (testato solo contro un chip simulato; non può armare né far partire un laser).
- **[HYDRA-UMC-BRIDGE-OPENPNP](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-OPENPNP)** — coordinatore ad alto livello sicuro per il flusso schede del pick-and-place OpenPnP.
- **[HYDRA-UMC-BRIDGE-PRINTER3D](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-PRINTER3D)** — barriera di coordinamento sicura per stampanti 3D Moonraker/Klipper, con comandi di lavoro reali e controllati.
- **[HYDRA-UMC-BRIDGE-ROS2](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-ROS2)** — coordinatore con un trasporto ROS 2 rclpy importato in modo lazy (non ancora eseguito contro un'installazione ROS 2).
- **[HYDRA-UMC-BRIDGE-UAV](https://github.com/JuanenRac/HYDRA-UMC-BRIDGE-UAV)** — barriera di coordinamento per UAV dotati di fotocamera, con un mittente di comandi MAVLink (testato solo contro un emulatore).

*Piattaforma Strumenti URTC*
- **[URTC](https://github.com/JuanenRac/URTC)** — firmware per la scheda fisica dell'Universal Robot Tool Controller, oltre 25 profili utensile su bus CAN.
- **[URTC-FLASHER](https://github.com/JuanenRac/URTC-FLASHER)** — strumento desktop con GUI per il flashing delle schede URTC, CAN-OTA più SWD/JTAG a chip intero.
- **[URTC-TESTER](https://github.com/JuanenRac/URTC-TESTER)** — strumento desktop di diagnostica CAN-bus dal vivo per schede URTC, un pannello per profilo utensile.
- **[URTC-WEB-STUDIO](https://github.com/JuanenRac/URTC-WEB-STUDIO)** — alternativa basata su browser a URTC-TESTER tramite la Web Serial API, senza installazione locale.
- **[URTC-GATEWAY](https://github.com/JuanenRac/URTC-GATEWAY)** — gateway davanti al bus CAN di URTC: elenco di identificatori consentiti, limite di frequenza e chiave di sessione; software provato su computer, non ancora su un bus reale.

*Nodo IA Visione (Hailo-8)*
- **[HYDRA-UMC-VISION-NODE](https://github.com/JuanenRac/HYDRA-UMC-VISION-NODE)** — hub di integrazione per la pipeline di visione Hailo-8, con un vero controllo di prontezza hardware per fase.
- **[HYDRA-UMC-DETECTION-HEF](https://github.com/JuanenRac/HYDRA-UMC-DETECTION-HEF)** — registro reale di modelli compilati con verifica di caricamento sicuro per architettura Hailo/checksum.
- **[HYDRA-UMC-VISION-STREAMER](https://github.com/JuanenRac/HYDRA-UMC-VISION-STREAMER)** — generatore reale di pipeline GStreamer + config MediaMTX, con una vera barriera di integrazione HailoRT.
- **[HYDRA-UMC-VISUAL-SERVOING-API](https://github.com/JuanenRac/HYDRA-UMC-VISUAL-SERVOING-API)** — legge di correzione Position-Based Visual Servoing con autorizzazione condizionata allo stato di zona; ancora nessuna stima di posa da telecamera.
- **[HYDRA-UMC-SAFETY-ZONES](https://github.com/JuanenRac/HYDRA-UMC-SAFETY-ZONES)** — logica v0 di violazione zona, freschezza di calibrazione e richiesta E-STOP che legge le posizioni degli oggetti da un file; nessun rilevatore la alimenta e nessun E-STOP fisico è ancora collegato.

*Nodo IA Cognitivo (Hailo-10)*
- **[HYDRA-UMC-VLA-ENGINE](https://github.com/JuanenRac/HYDRA-UMC-VLA-ENGINE)** — vera codifica/decodifica di token d'azione e generazione di traiettoria per un modello Vision-Language-Action.
- **[HYDRA-UMC-VOICE-UI](https://github.com/JuanenRac/HYDRA-UMC-VOICE-UI)** — vero front-end vocale (VAD + parser di intenti) con un relay verso Watch limitato e soggetto a conferma.
- **[HYDRA-UMC-SEMANTIC-PLANNER](https://github.com/JuanenRac/HYDRA-UMC-SEMANTIC-PLANNER)** — vera scomposizione dei task basata su regole e recupero semantico degli errori sui codici errore MCU.
- **[HYDRA-UMC-DOCS-QA](https://github.com/JuanenRac/HYDRA-UMC-DOCS-QA)** — vera ricerca documentale TF-IDF (solo libreria standard) sui documenti Markdown di questo ecosistema.
- **[HYDRA-UMC-LOCAL-TECHNICIAN](https://github.com/JuanenRac/HYDRA-UMC-LOCAL-TECHNICIAN)** — tecnico di manutenzione IA locale e con cancelli di policy per l'ecosistema stesso - osserva, diagnostica e propone correzioni; i due livelli di rischio più alti non sono deliberatamente ancora implementati.

*Gemello Digitale e Simulazione*
- **[HYDRA-UMC-TWIN](https://github.com/JuanenRac/HYDRA-UMC-TWIN)** — hub di integrazione per il motore di gemello digitale, con un vero contratto di sincronizzazione per compatibilità di versione.
- **[HYDRA-UMC-HIL-BRIDGE](https://github.com/JuanenRac/HYDRA-UMC-HIL-BRIDGE)** — router hardware-in-the-loop con un interblocco software di collisione tra simulazione e hardware reale (testato senza hardware reale).
- **[HYDRA-UMC-PHYSICS-REPLICA](https://github.com/JuanenRac/HYDRA-UMC-PHYSICS-REPLICA)** — vera cinematica diretta e validazione dei limiti articolari su un vero sottoinsieme URDF.
- **[HYDRA-UMC-SYNTHETIC-DATA-GEN](https://github.com/JuanenRac/HYDRA-UMC-SYNTHETIC-DATA-GEN)** — vero generatore procedurale di scene 2D con esportazione di annotazioni YOLO/COCO.

*Dati e Analisi*
- **[HYDRA-UMC-DATALAKE](https://github.com/JuanenRac/HYDRA-UMC-DATALAKE)** — vero archivio di serie temporali basato su sqlite3, con una vera API HTTP di ingestione/query.
- **[HYDRA-UMC-ANOMALY-DETECTOR](https://github.com/JuanenRac/HYDRA-UMC-ANOMALY-DETECTOR)** — vero rilevatore di anomalie FFT + baseline statistica, con monitoraggio della deriva.
- **[HYDRA-UMC-PRODUCTION-REPORTS](https://github.com/JuanenRac/HYDRA-UMC-PRODUCTION-REPORTS)** — vero calcolo OEE/disponibilità sullo storico di DATALAKE, con esportazione CSV riproducibile.
- **[HYDRA-UMC-TELEMETRY-COLLECTOR](https://github.com/JuanenRac/HYDRA-UMC-TELEMETRY-COLLECTOR)** — vera pipeline di ingestione CAN/WebSocket verso DATALAKE, con deduplicazione per sequenza.

*Gateway Industriale*
- **[HYDRA-UMC-GATEWAY-INDUSTRIAL](https://github.com/JuanenRac/HYDRA-UMC-GATEWAY-INDUSTRIAL)** — hub di integrazione che inoltra ai protocolli industriali, con un vero livello di allowlist dei comandi/backpressure.
- **[HYDRA-UMC-OPCUA-SERVER](https://github.com/JuanenRac/HYDRA-UMC-OPCUA-SERVER)** — vero spazio di indirizzi OPC-UA, verificato con una vera sessione client del protocollo binario.
- **[HYDRA-UMC-MQTT-BROKER](https://github.com/JuanenRac/HYDRA-UMC-MQTT-BROKER)** — vero broker MQTT con autenticazione opzionale per client e ACL sui topic.
- **[HYDRA-UMC-MTCONNECT-ADAPTER](https://github.com/JuanenRac/HYDRA-UMC-MTCONNECT-ADAPTER)** — veri endpoint XML `/probe` e `/current` di MTConnect, con output in modalità degradata.

*Strumenti Complementari e Operazioni dell'Ecosistema*
- **[HYDRA-UMC-DASHBOARD-AI](https://github.com/JuanenRac/HYDRA-UMC-DASHBOARD-AI)** — pannelli Smart Summaries e Anomaly Highlighting su DATALAKE/ANOMALY-DETECTOR, con un fallback statistico onesto.
- **[HYDRA-UMC-TOOL-CLI](https://github.com/JuanenRac/HYDRA-UMC-TOOL-CLI)** — CLI di flotta con un vero e stabile contratto di exit-code, un client live reale della stessa API di HYDRA-UMC-SERVER.
- **[HYDRA-UMC-WATCH](https://github.com/JuanenRac/HYDRA-UMC-WATCH)** — app companion WearOS con schemi di avviso aptico e un relay vocale verso il telefono abbinato (non ancora eseguita su un orologio fisico).
- **[URTC-SMART-RACK](https://github.com/JuanenRac/URTC-SMART-RACK)** — firmware per un rack di montaggio schede con decodifica reale dell'ID utensile e logica di preriscaldamento Smart Idle.
- **[URTC-VISION-TOOL](https://github.com/JuanenRac/URTC-VISION-TOOL)** — firmware più un vero companion di visione Python per una testa utensile di ispezione termica/RGB.
- **[HYDRA-UMC-UPDATER](https://github.com/JuanenRac/HYDRA-UMC-UPDATER)** — strumento amministrativo desktop che scopre, clona e aggiorna ogni repository di questo ecosistema.
- **[HYDRA-UMC-OS-REBUILDER](https://github.com/JuanenRac/HYDRA-UMC-OS-REBUILDER)** — strumento desktop Windows/Linux che costruisce un'immagine della CM5 pronta da scrivere, precaricata con le versioni più aggiornate dell'ecosistema, con configurazione di primo avvio Wi-Fi/utente/SSH in stile Raspberry Pi Imager.
- **[HYDRA-UMC-OPS-AGENT](https://github.com/JuanenRac/HYDRA-UMC-OPS-AGENT)** — coordinatore di incidenti di manutenzione: un ruolo edge a basso privilegio raccoglie uno snapshot di inventario/salute sanificato, un ruolo control-plane lo rende in sola lettura e chiede a un provider di IA di suggerire una diagnosi - non applica mai una patch né distribuisce nulla.
- **[HYDRA-UMC-DEV-SERVER](https://github.com/JuanenRac/HYDRA-UMC-DEV-SERVER)** — host di sviluppo riproducibile (Raspberry Pi 5 / CM5) che conserva il codice sorgente dell'ecosistema ed esegue attività delimitate di build/test tramite una coda durevole; un ruolo di sviluppo dedicato, esplicitamente distinto da un CM5 operativo.


---

## 📚 Documentazione e Comunità

- **[CONTRIBUTING.md](CONTRIBUTING.md)** — stack tecnologico e linee guida di codifica per una pull request.
- **[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)** — gli standard di comportamento attesi in questa comunità.
- **[SECURITY.md](SECURITY.md)** — come segnalare una vulnerabilità, e le reali aree di attenzione sulla sicurezza di questo progetto.
- **[SUPPORT.md](SUPPORT.md)** — dove porre domande e segnalare bug.
- **[LICENSE.md](LICENSE.md)** — la licenza propria di questo progetto.

## 👤 AUTORE
**JuanenRac** (Electro Hobby 3D)
📧 electrohobby3d@gmail.com
📺 [youtube.com/@electrohobby3d](https://youtube.com/@electrohobby3d)

## 📜 LICENZA
GPL-3.0-or-later - Vedere LICENSE per i dettagli.
