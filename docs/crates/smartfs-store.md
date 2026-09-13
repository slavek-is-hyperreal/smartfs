# smartfs-store — delta po ADR-58

← [Mapa crate'ów](../02-crates.md) | Baza: [SmartFS_Architecture_v4_5.md](../base-v4.5-v5.0/SmartFS_Architecture_v4_5.md) §3.3 „smartfs-store/CLAUDE.md" | Powód wydzielenia z [_unchanged.md](_unchanged.md): [ADR-58](../adr/ADR-58-two-stage-cow-commit.md)

Do ADR-58 ten crate był „bez zmian w v6.0": `BlobStore` trait plus `LocalDiskStore`, przyjmujące już skompresowane bajty, bez wiedzy o SQL, hashowaniu i FUSE. ADR-58 dokłada dwa komponenty, które nie mieszczą się nigdzie indziej, bo oba są operacjami **na katalogu store'u**, a nie na bazie.

## 1. `pending/` — kolejka trwałości pierwszego etapu

Layout, pod tym samym `store_path` co bloby, żeby `rename()` nigdy nie przekraczał granicy systemu plików:

```
<store_path>/pending/tmp/     częściowo zapisane znaczniki, drenaż ich nie czyta
<store_path>/pending/queue/   trwałe znaczniki, <seq>_<content_hash>.json
```

Wzorzec Maildira: zapis całości do `tmp/`, `sync_all`, `rename()` do `queue/`. `rename()` w obrębie jednego systemu plików jest atomowy na poziomie jądra — drenaż nigdy nie zobaczy połowy znacznika, a crash w trakcie zapisu zostawia tylko śmieć w `tmp/`. **Ta atomowość JEST gwarancją trwałości** — świadomie nie ma tu własnego formatu WAL (patrz §Odrzucone alternatywy w ADR-58).

```rust
pub struct PendingQueue { /* tylko ścieżki, tanie w klonowaniu */ }

impl PendingQueue {
    pub fn new(store_root: impl AsRef<Path>) -> Self;
    pub async fn ensure_dirs(&self) -> Result<()>;
    pub async fn enqueue(&self, marker: &PendingMarker) -> Result<PathBuf>;
    pub async fn list(&self) -> Result<Vec<String>>;   // FIFO
    pub async fn read(&self, file_name: &str) -> Result<Option<PendingMarker>>;
    pub async fn remove(&self, file_name: &str) -> Result<()>;  // idempotentne
    pub async fn depth(&self) -> Result<usize>;
    pub async fn sweep_tmp(&self, older_than_secs: u64) -> Result<usize>;
}
```

Dwie decyzje, które warto znać, zanim się to zmieni:

- **`seq` w nazwie jest wypełniony zerami do 20 cyfr** (szerokość `u64::MAX`), więc zwykłe sortowanie leksykograficzne wyniku `readdir` daje kolejność FIFO — bez `stat` na każdy wpis.
- **Nazwa, która się nie parsuje, jest pomijana; treść, która się nie parsuje, jest błędem.** Obcy plik w `queue/` nigdy nie może zostać odtworzony jako zapis; ale znacznik o poprawnej nazwie i zepsutej treści to realne dane, których nie umiemy odczytać, i musi być głośny.

Sam `PendingMarker` mieszka w [`smartfs-schema`](smartfs-schema.md), nie tutaj — potrzebują go obie strony (FUSE zapisuje, drenaż czyta), a ten crate ma kontrakt „zero wiedzy o bazie".

## 2. `scrub` — sumy kontrolne zatwierdzonych blobów

**To nie jest skan `pending/`.** Wyglądają podobnie i są swoimi przeciwieństwami:

| | szuka | znajduje |
|---|---|---|
| skan `pending/queue/` | brakującego wiersza dla istniejącego pliku | niezacommitowanego zapisu |
| `scrub` | zepsutego pliku dla istniejącego wiersza | bit rot |

ADR-58 wprost zabrania mylić je w implementacji i w logach, więc każdy komunikat tego komponentu mówi `scrub`, nigdy `scan`.

```rust
pub async fn scrub_once<F>(
    store_root: impl AsRef<Path>,
    expectations: &[(Uuid, String)],   // blob_id -> content_hash z bazy
    scope: ScrubScope,                 // Full | Sample(n)
    pacing: ScrubPacing,               // pauza między blobami
    decode: F,                         // bajty ze store'u -> digest plaintextu
) -> Result<ScrubReport>
where F: Fn(&[u8]) -> std::result::Result<String, String>;
```

**Dlaczego kodek jest wstrzykiwany, a nie wołany wprost:** `smartfs-compress` już zależy od `smartfs-store`, więc zależność zwrotna byłaby cyklem. Wołający podaje domknięcie zamieniające bajty na digest, a ten moduł trzyma tylko to, co należy do niego: chodzenie po katalogu, próbkowanie, tempo i raport. Oczekiwane digesty przychodzą jako dane (`smartfs_db::list_blob_digests`), więc crate zostaje też bez SQL-a.

Trzy kształty awarii są rozróżniane, bo znaczą co innego dla operatora: `Digest` (odkodowało się, ale do innej treści — cicha korupcja), `Undecodable` (uszkodzenie strukturalne), `Missing` (baza referuje plik, którego nie ma).

**Znaleziska są raportowane, nigdy naprawiane.** Poprawne bajty nie są stąd znane, a skasowanie zepsutego bloba zamieniłoby wykrywalny bit rot w brakujący plik. Pliki bez referencji są liczone, nie ruszane — to kandydaci do GC, a ta decyzja nie należy do scrubu. `pending/` jest przy tym liczeniu pomijany, inaczej każdy zapis w locie byłby raportowany jako wyciek.

## Czego ADR-58 tu NIE zmienia

`BlobStore` trait, `LocalDiskStore::put/get/delete/exists`, atomowy zapis blobów przez write-then-rename, FIX-03 (`store.exists` na ścieżce dedupu) — bez zmian. Uwaga z `_unchanged.md`, że `store.exists()` dokłada round-trip dla backendów sieciowych, nadal obowiązuje i nadal jest poza zakresem.
