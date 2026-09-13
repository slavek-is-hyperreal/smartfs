# smartfs-fuse — delta po ADR-58

← [Mapa crate'ów](../02-crates.md) | Baza: [SmartFS_Architecture_v4_5.md](../base-v4.5-v5.0/SmartFS_Architecture_v4_5.md) §3.6 „smartfs-fuse/CLAUDE.md" | Powód wydzielenia z [_unchanged.md](_unchanged.md): [ADR-58](../adr/ADR-58-two-stage-cow-commit.md)

Do ADR-58 ten crate był „bez zmian w v6.0". ADR-58 przepisuje jego najgorętszą ścieżkę: `release()` przestaje czekać na transakcję Postgresową.

## Co się zmieniło w `release()`

Przedtem: bufor RAM → hash → dedup przez `blobs` → kompresja → `store.put` → **`cow_commit`** → odpowiedź do wołającego.

Teraz: bufor RAM → hash → dedup → kompresja → `store.put` → **trwały znacznik `pending`** → odpowiedź do wołającego. Transakcję wykonuje w tle pojedynczy konsument.

KROK 1 jest bez zmian — dedup, kompresja, `store.put`, leczenie FIX-03, kompensacja FIX-04 nadal dzieją się synchronicznie, bo bez bloba na dysku nie ma czego potwierdzać. Zmienia się dopiero KROK 2.

## Trzy rzeczy, które są nośne, a wyglądają na szczegół

**1. Kolejność potwierdzenia.** Punkt 2 Decyzji ADR-58 mówi, że referencja trafia do kolejki RAM *po* powrocie z `release()`, a punkt 6, że pełna kolejka blokuje wołającego zamiast gubić zapis. Godzi je tylko jedna kolejność: **najpierw zajmij slot** (blokując, z limitem z §Rozstrzygnięcia #1), potem `rename()`, potem potwierdź, potem oddaj nazwę. Zajęcie slotu na końcu oznaczałoby odmowę zapisu, który już obiecaliśmy zachować.

**2. Co liczy limit.** Nie zajętość kanału do drenażu, tylko **znaczniki przyjęte i jeszcze nieodhaczone**. Przy limicie na kanale drenaż, który *szybko pada* (Postgres leży), zwalnia slot przy każdym błędzie — więc zapisy są dalej przyjmowane, a znaczniki rosną na dysku bez ograniczeń, czyli dokładnie ta ucieczka, przed którą punkt 6 ma chronić. Licząc nieodhaczone znaczniki, padający drenaż wywiera taki sam nacisk zwrotny jak wolny. Licznik jest zasiewany przy starcie tym, co już leży w `pending/queue/` — restart w zaległość nie dostaje świeżego przydziału, a zasiew powyżej limitu jest zamierzony: zapisy zostają odrzucane, aż zaległość zejdzie poniżej.

**3. Skan nigdy nie commituje sam.** Startowy i okresowy skan tylko dokarmiają jedno zadanie drenażu. To utrzymuje „jeden konsument sekwencyjny" z punktu 3 dosłownie prawdziwym — a z nim własność, że `version_number` liczony per transakcja idzie w kolejności zapisów — i całkiem usuwa okno na równoległy replay, w którym dwa zadania odhaczyłyby ten sam znacznik i dwa razy zwolniły jego slot.

**4. Odczyt nie może zostać w tyle za potwierdzeniem.** Skoro `release()` potwierdza przed commitem, `inode_registry` w tym oknie opisuje poprzednią wersję — a świeży `open()` ma pusty bufor i spada właśnie do bazy. Bez nakładki `echo x > f; cat f` zwracałoby starą treść, a `stat` stary rozmiar, temu samemu procesowi, który przed chwilą pisał. Dlatego każdy nieodhaczony znacznik publikuje pod swoim inode `(blob_id, size, content_hash)`, a `read()` i `getattr()` zaglądają tam przed bazą. Wycofanie wpisu jest kluczowane nazwą znacznika, więc zakończenie starszego commitu nie skasuje nowszego zapisu, a skan startowy odbudowuje nakładkę, zanim demon ogłosi gotowość. Patrz [ADR-58 punkt 7 Decyzji](../adr/ADR-58-two-stage-cow-commit.md), dopisany w trakcie implementacji.

## Nowy publiczny interfejs

```rust
pub struct PendingLimits { pub max_entries: usize, pub block_timeout: Duration }
impl PendingLimits { pub fn from_env() -> Self; pub fn entries_for_ram(total: u64) -> usize; }

pub struct PendingPipeline { /* ... */ }
impl PendingPipeline {
    pub async fn start(pool, store_root, rt, limits) -> Result<Self>;  // skan startowy w środku
    pub async fn submit(&self, marker: &PendingMarker) -> Result<()>;
    pub async fn rescan(&self) -> Result<usize>;
    pub async fn quiesce(&self, timeout: Duration) -> bool;
    pub fn ram_depth(&self) -> usize;
    pub fn view_of(&self, inode_id: Uuid) -> Option<PendingView>;  // nakładka odczytu
    pub fn overlay_depth(&self) -> usize;
    pub fn next_seq(&self) -> u64;
    pub fn queue(&self) -> &PendingQueue;
}
```

`SmartFsFuse::new` przyjmuje `PendingPipeline` jako argument, nie `Option`. System plików z dwiema ścieżkami zapisu to system plików, którego zachowanie przy craśhu zależy od tego, jak go skonstruowano — świadomie nie ma trybu awaryjnego z synchronicznym `cow_commit`.

Skan startowy jest **wewnątrz** `start()` i kończy się, zanim funkcja wróci, więc demon nie może ogłosić gotowości, gdy zaległość leży niezakolejkowana.

## Strojenie

| Zmienna | Domyślnie | Znaczenie |
|---|---|---|
| `SMARTFS_PENDING_QUEUE_MAX` | ułamek RAM-u, sufit 100 000, podłoga 1 024 | ile nieodhaczonych znaczników wolno |
| `SMARTFS_PENDING_BLOCK_TIMEOUT_MS` | 30 000 | ile `write()` czeka, zanim odmówi |
| `SMARTFS_PENDING_SCAN_INTERVAL_MS` | 60 000 | co ile okresowy skan wznawia nieudane |

Czytane **raz, przy starcie**. Punkt 5 Decyzji jest wprost przeciw przeliczaniu limitu wobec chwilowo wolnej pamięci — pod presją z zewnątrz byłby nieaktualny, zanim zdążyłby zadziałać.

## Nowy kod błędu

`SmartFsError::PendingQueueFull` → `EAGAIN` w `error_to_errno`. Znaczy „nie przyjąłem", nigdy „przyjąłem i zgubiłem": w momencie odmowy nic nie zostało utrwalone ani potwierdzone.

Wybrano blokadę z limitem, nie bez limitu, bo jądro Linuksa nie ma domyślnego timeoutu na żądania FUSE — blokada bez limitu to proces w stanie `D`, odporny na `kill -9`, i mount nie do odmontowania. Pełne uzasadnienie: [ADR-58 §Rozstrzygnięcia #1](../adr/ADR-58-two-stage-cow-commit.md).

## Czego ADR-58 tu NIE zmienia

Bufor RAM w `write()` (ADR-16), rozdział `flush()`/`release()`, walidacja składni i `EACCES` przy błędzie, opóźniony unlink, mapowanie inode'ów, `default_mount_options()` — bez zmian. Nadal zero SQL wprost: `smartfs-fuse` deleguje do publicznego API `smartfs-db`, a `commit_pending_marker` mieszka właśnie tam (żeby `smartfs-cli` mógł odtwarzać kolejkę bez zależności od `fuser`) i jest stąd tylko reeksportowany.
