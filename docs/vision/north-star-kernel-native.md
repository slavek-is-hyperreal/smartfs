# North Star — SmartFS jako natywny system plików jądra (Pick OS + agenci)

**To NIE jest ADR.** ADR to decyzja + powód dla czegoś, co wchodzi do specyfikacji. To jest notatka robocza z wizji dalekiej przyszłości — spisana, żeby wątek nie zniknął, nie po to, żeby cokolwiek z tego wchodziło do Faz 0-6 albo do promptu dla Antigravity. Nic tu nie jest zdecydowane; wszystko jest otwartym pytaniem do podjęcia, kiedy demon na CPU już działa i to naprawdę stanie się aktualne.

## Cel końcowy

Własna, zmodyfikowana dystrybucja (bazowana na Linux Mint) z jądrem, w którym SmartFS działa natywnie jako główny system plików (root), nie jako FUSE w przestrzeni użytkownika. System operacyjny, w którym słownik danych, wersjonowanie i agentowość są częścią ontologii jądra, nie dodatkiem nad nim — "Pick OS + agenci" jako punkt wyjścia, nie punkt docelowy dokładany później.

## Dlaczego to nie jest naiwne — stan faktyczny (sprawdzony, wrzesień 2026)

Rust w jądrze Linuksa jest produkcyjny na poziomie sterowników: Binder (Android IPC) w produkcyjnym AOSP, sterownik GPU Apple'a scalony od 6.11, abstrakcje NVMe, framework PHY sieciowych od 6.8, sterowniki GPIO. To nie jest "może kiedyś" — to działa dziś.

Na poziomie systemów plików jest wcześniej: istnieje prototyp sterownika EXT2 w Rust (~600 linii, tylko-do-odczytu), demonstrujący że nowa abstrakcja Rust VFS działa, ale nic nie jest jeszcze scalone do głównego jądra. Ogólne bindingi Rust-FUSE po stronie jądra są "w budowie" wg materiałów z 2026. Wniosek: wejście istnieje i dojrzewa, ale nikt jeszcze nie wypuścił produkcyjnego natywnego systemu plików w Rust. Ta wizja siedzi dokładnie na granicy dzisiejszego frontu, nie w science fiction — ale też nie jest to coś na przyszły miesiąc.

Sources: [Rust VFS / EXT2 driver — Phoronix](https://www.phoronix.com/news/Rust-VFS-Linux-V2-Now-With-EXT2), [Rust in the Linux Kernel 2026 — Rustify](https://rustify.rs/articles/rust-in-linux-kernel-2026)

## Kluczowy reframe: "natywny" ≠ "Postgres w jądrze"

Pełny RDBMS (stos sieciowy, background workery, WAL) nie może i nie powinien żyć w kernel space. To nie jest problem specyficzny dla SmartFS — Linux rozwiązuje ten sam kształt problemu od dawna dla NFS, CIFS/SMB, 9p, virtiofs: cienki natywny klient protokołu w jądrze (integracja z VFS, page cache), a cała stanowa logika żyje w userspace (albo na zdalnym hoście).

Zastosowanie do SmartFS: `smartfs-fuse` dostaje kiedyś natywnego kernelowego brata, mówiącego tym samym protokołem do **dokładnie tego samego, niezmienionego demona** (Postgres + `smartfs-db` + `smartfs-ai` + `smartfs-semantic` + `smartfs-mcp`) przez szybszy kanał niż FUSE (netlink / ioctl / coś na io_uring) zamiast przez userspace↔kernel round-trip protokołu FUSE. **Wszystkie ADR-y 01-57 przeżywają to przejście praktycznie nietknięte** — zmienia się tylko to, co dziś robi `smartfs-fuse`, nic więcej w stosie.

## Najtrudniejszy realny problem: bootstrapping roota

Root filesystem musi być czytelny dla bootloadera i montowalny, zanim jakikolwiek userspace (a więc Postgres i demon SmartFS) w ogóle wystartuje — jajko i kura. Btrfs/ZFS-jako-root rozwiązują to tym samym sposobem: mała konwencjonalna partycja `/boot` (GRUB-czytelna), właściwy root montowany dopiero po starcie jądra. Dla SmartFS to nie wystarczy samo w sobie — Invariant #3 (jedyna legalna droga do danych wiedzie przez daemona) oznacza, że potrzebny jest jawny, dwustopniowy tryb: "głupi" (surowy dostęp do blobów przez ext4, zanim Postgres wstanie) i "smart" (pełna warstwa CoW/wersjonowania/semantyki, gdy demon i baza już żyją).

## Rozwiązanie bootstrapu: "early userspace" (nazwane przez autora "Linux w Linuxie")

To NIE jest egzotyczna technika — to ten sam ~15-letni wzorzec, na którym stoi każdy nowoczesny Linux dla LUKS, LVM, software RAID, root-po-sieci: mały, tymczasowy Linux w RAM-dysku (initramfs) montuje to, co potrzeba, zanim właściwy root przejmie stery.

Trzy warianty, uszeregowane od najprostszego/najsolidniejszego do najbardziej kruchego:

1. **`switch_root`/`pivot_root` + restart Postgresa nad trwałym stanem (rekomendowane jako główny mechanizm).** Initramfs montuje zwykły ext4, startuje na nim Postgresa + demona SmartFS, czeka aż będzie gotowy, `switch_root` na docelowy root. "Przejęcie stanu" = czysty restart Postgresa nad tym samym katalogiem danych (WAL gwarantuje bezpieczny restart), NIE żywa transplantacja procesu. Nie wymaga wynajdywania nowego mechanizmu jądra — Postgres i tak jest projektowany pod dokładnie taki restart.
2. **Namespace/kontener (systemd-nspawn-style).** Postgres + demon działają cały czas w izolowanym poddrzewie procesów, nigdy nie są "przejmowane" — właściwy system po prostu montuje SmartFS przez wciąż żywy, nadzorowany serwis. Rozważyć, jeśli restart Postgresa przy każdym boot (replay WAL) okaże się zbyt kosztowny czasowo przy dużej bazie.
3. **Zagnieżdżone jądro (KVM nested) + CRIU (żywa migracja procesu).** Realna, istniejąca technologia (CRIU, używane przez OpenVZ), ale najbardziej krucha — otwarte gniazda sieciowe, pamięć współdzielona, stan sprzętowy potrafią się nie odtworzyć czysto. Warte rozważenia tylko, gdyby pojawił się konkretny powód chcieć prawdziwej izolacji jądra (bezpieczeństwo, twarda granica), nie tylko jako rozwiązanie samego bootstrapu.

## Sekwencjonowanie bootu jako graf zależności (nie ręczny scheduler)

Boot to graf zależności (DAG), nie łańcuch — dokładnie ten sam kształt problemu, który `docs/06-agentic-execution-plan.md` już rozwiązuje dla sekwencji budowy crate'ów. Nie trzeba tego wynajdywać dla bootstrapu SmartFS — `systemd` już to robi produkcyjnie: `After=`/`Before=` (kolejność) i `Requires=`/`Wants=` (konieczność) to dwie rozdzielone osie grafu; systemd startuje równolegle wszystko, co ma spełnione zależności w danym momencie (sortowanie topologiczne + harmonogramowanie na froncie fali). `systemd-analyze critical-chain` liczy ścieżkę krytyczną (matematyka PERT/CPM) — absolutne minimum czasu bootu niezależnie od liczby rdzeni; `systemd-analyze plot`/`dot` eksportuje graf do Graphviza.

**Konkretna implikacja dla SmartFS:** gotowość Postgresa/demona po `switch_root` powinna być wyrażona jako zwykły target systemd (np. `smartfs-postgres-ready.target`), od którego zależą kolejne jednostki — zamiast ręcznie pisanej logiki sekwencjonowania. Daje to równoległość i analizę wąskich gardeł za darmo.

**Technika warta zapamiętania — socket activation:** systemd potrafi utworzyć gniazdo nasłuchujące usługi natychmiast (tanie), pozwolić zależnym usługom ruszyć równolegle, i dopiero pierwsze połączenie budzi pełny proces (jądro buforuje żądanie w międzyczasie). To usuwa z grafu krawędzie, które są tylko konserwatywnym uproszczeniem, nie prawdziwą koniecznością — warte rozważenia przy `smartfs-postgres-ready.target`, jeśli okaże się, że część "zależnych" usług tak naprawdę czeka na gniazdo, nie na w pełni gotowego Postgresa.

## Pochodzenie tego wątku (żeby nie zgubić kontekstu)

System pluginów (`plugins/*.json`) powstał świadomie po materiale o Pick OS (kanał Asianometry) już w okolicach v3.0 specyfikacji — nie jest przypadkową zbieżnością odkrytą post factum (patrz [ADR-57](../adr/ADR-57-pick-style-plugin-dictionary.md)). Ta rozmowa o jądrze jest naturalnym przedłużeniem tej samej inspiracji na cały system operacyjny, oraz uogólnieniem wizji self-hostingu, którą projekt już ma zapisaną dla samego kodu źródłowego (`docs/04-uuid-doc-linking.md`: "gdy self-hosting wyląduje, `smartfs-docgen` powinien zostać usunięty na rzecz bezpośredniego odpytywania własnej bazy SmartFS-a o samego siebie").

## Otwarte pytania (świadomie nierozstrzygnięte)

- Który z trzech wariantów bootstrapu (switch_root / namespace / nested+CRIU) ostatecznie, gdy poznamy realne ograniczenia czasowe/sprzętowe.
- Jak dokładnie wystawić "tryb głupi" (przed demonem) aplikacjom, które mogłyby chcieć czegoś w oknie między startem jądra a pełną gotowością warstwy semantycznej.
- Czy GRUB (albo inny bootloader) potrzebuje kiedykolwiek własnej świadomości SmartFS, czy mała konwencjonalna partycja `/boot` wystarczy na zawsze.
- Kiedy (jeśli w ogóle) `smartfs-fuse` faktycznie dostaje natywnego kernelowego brata, biorąc pod uwagę, że sam Rust-VFS w jądrze jest dziś na etapie prototypu do odczytu.
