# ADR-55 — Akceleracja GPU: Vulkan jako jedyna droga, reuse-before-rewrite

← [Mapa ADR](../01-architecture.md)

**Status:** Przyjęty do implementacji w v6.0. **Rewizja 2.** Rewizja 1 rekomendowała `CubeCL` (pisanie własnych kerneli) jako fundament, a gotowe rozwiązania (`llama.cpp`/`ggml`) traktowała wyłącznie jako punkt odniesienia, odrzucając je, bo "kernele nie są nasze". Ta rewizja to odwraca: celem nadrzędnym jest otwartoźródłowa, niezależna od wendora akceleracja — nie czystość autorstwa kodu. Tam, gdzie taki cel już realizuje dojrzały projekt open-source, SmartFS go przyjmuje; pisanie własnych kerneli jest zarezerwowane na to, czego żaden gotowy projekt nie pokrywa.

## Kontekst — co się zmieniło od rewizji 1

Nadrzędna zasada: **custom compute kernele są potrzebne tam, gdzie nie ma szybkiego, gotowego, otwartoźródłowego rozwiązania — nie wszędzie z zasady.** Cel projektu to nie "SmartFS pisze każdy kernel sam", tylko "SmartFS nigdy nie zależy od zamkniętego, jednowendorowego stacku obliczeniowego (CUDA, ROCm/HIP)" i "przyspieszenie działa niezależnie od wendora karty". `ggml` (silnik `llama.cpp`) ma dojrzały, szeroko używany backend Vulkan — pisanie własnego odpowiednika matmul/attention/RMSNorm od zera, skoro taki już istnieje, przetestowany i wspierający dokładnie nasz model (`Qwen3-Embedding-0.6B`, ADR-49), byłoby marnotrawstwem, nie cnotą.

Dodatkowy wymiar celu, niedoceniony w rewizji 1: Vulkan, w odróżnieniu od CUDA/ROCm, jest zaimplementowany szerzej niż tylko na desktopowych kartach graficznych — obejmuje GPU w Raspberry Pi (sterownik `V3DV`/Mesa, zgodność Vulkan 1.3 scalona w Mesa 24.3) i w smartfonach (Adreno, Mali, przez sterowniki producentów). To realnie rozszerza krąg sprzętu, na którym projekt może kiedyś przyspieszać — nie tylko wendorów desktopowych kart.

## Zbadane fakty wydajnościowe (żeby decyzja nie była wyłącznie ideologiczna)

"Tylko Vulkan" ma realny, zmienny koszt wydajnościowy w zależności od sprzętu — trzeba go nazwać uczciwie:

- **Desktop AMD** (RX 7900 XTX, benchmarki społecznościowe `llama.cpp`): ROCm bije Vulkan o ok. +44% w generacji tekstu i ok. +202% (~3×) w przetwarzaniu promptu. Powód architektoniczny: ROCm korzysta z `rocBLAS` — biblioteki BLAS dostrojonej pod konkretny sprzęt AMD; generyczny backend Vulkan w `ggml` używa mniej wyspecjalizowanych kerneli liniowej algebry. To jest realna cena zasady "zero ROCm", zaakceptowana tu świadomie, nie przeoczona.
- **Desktop NVIDIA** (RTX 3080, dyskusja `ggml-org/llama.cpp#10879`): podobny wzorzec, CUDA ok. 2–2,6× szybsze niż Vulkan na tej samej karcie. Nieistotne dla decyzji SmartFS, bo CUDA i tak nigdy nie jest tu opcją.
- **Smartfony** (Adreno, Mali, przez backend Vulkan w `llama.cpp`): to nie jest drobna strata, tylko **nierozwiązany, poważny regres**. Zgłoszenia społeczności (`ggml-org/llama.cpp#9464`, stan na luty 2026) pokazują GPU-przez-Vulkan ok. **15× wolniejsze** niż zwykłe CPU na tym samym telefonie, na wielu chipsetach (Snapdragon 8 Gen 3, RK3588) — przyczyna niezdiagnozowana w projekcie `ggml`. Konkurencyjne projekty (MLC-LLM, MediaPipe) radzą sobie z akceleracją GPU na Androidzie lepiej, co sugeruje ograniczenie konkretnej implementacji `llama.cpp`, nie fundamentalną niemożliwość Vulkan-na-telefonie. **Dlaczego to zostaje nierozwiązane, a nie jest to lenistwo projektu:** powiązane zgłoszenia (`#5186`, `#12139`, `#20603`) pokazują, że to nie jeden bug, tylko rozproszony ogon problemów specyficznych dla konkretnych kombinacji wendor-sterownik-model karty (błędy kompilatora shaderów Adreno przy pewnych wzorcach sterowania przepływem, `ErrorDeviceLost` przy większych batchach, regresje na konkretnych SoC) — każde zgłoszenie kończy oznaczone `bug-unconfirmed`/`stale`, bo sterowniki Adreno/Mali są zamkniętymi blobami (w odróżnieniu od otwartego Mesa dla RPi/desktopu), więc realna diagnoza wymaga dezasemblacji kodu GPU i kogoś z rzadką wiedzą reverse-engineeringową (jak np. Rob Clark przy podobnym, rozwiązanym w 2021 przypadku wolnego shadera na Adreno — okazało się, że kompilator konserwatywnie zakładał możliwy konflikt spójności pamięci i emitował wolniejszą ścieżkę odczytu) — a projekt wolontariacki skupiony głównie na desktopowym NVIDIA/AMD/Metal nie ma kogoś takiego przypisanego do każdej kombinacji telefonu.
- **Raspberry Pi**, GPU wbudowane (VideoCore/`V3DV`): sterownik Vulkan istnieje i jest zgodny (Vulkan 1.3, Mesa 24.3), ale nie znaleziono wiarygodnych benchmarków realnego przyspieszenia inferencji LLM/embeddingów na tym konkretnym GPU względem CPU Pi — dostępne relacje o akceleracji LLM na Raspberry Pi 5 używają zewnętrznego GPU podpiętego przez PCIe, nie wbudowanego VideoCore. Traktować obecność sterownika jako "droga otwarta na przyszłość", nie jako potwierdzoną korzyść dziś.

**Wniosek:** "wszędzie tam, gdzie jest Vulkan" nie znaczy dziś "wszędzie szybciej niż CPU". Na słabych/wbudowanych GPU (telefon, RPi) CPU pozostaje praktycznym wyborem domyślnym już teraz. To nie podważa tej decyzji — ADR-55 (rewizja 1) już ustalił, że GPU jest bonusem, nigdy wymogiem: urządzenie, na którym GPU-przez-Vulkan wychodzi wolniejsze niż CPU, po prostu z niego nie korzysta. Nic się nie psuje, nic nie blokuje startu.

## Decyzja

1. **Zasada reuse-before-rewrite:** gdzie istnieje szybkie, dojrzałe, otwartoźródłowe rozwiązanie realizujące cel (akceleracja niezależna od wendora, przez Vulkan), SmartFS je przyjmuje zamiast pisać własny odpowiednik od zera. Pisanie custom kerneli jest zarezerwowane na to, czego żadne gotowe rozwiązanie nie pokrywa.
2. **Zastosowanie tej zasady do inferencji embeddingów:** `ggml`/`llama.cpp`, backend Vulkan, budowany jawnie **bez** `GGML_CUDA`/`GGML_HIP` (flagi wyłączone na poziomie CMake — binarka, którą SmartFS dystrybuuje, fizycznie nie zawiera kodu CUDA/ROCm, niezależnie od tego, że upstreamowy projekt oferuje je jako opcje budowania). Dostęp przez bindingi FFI w Rust (np. `llama-cpp-2`). `Qwen3-Embedding-0.6B` (domyślny model, ADR-49) ma oficjalny release GGUF (`Qwen/Qwen3-Embedding-0.6B-GGUF`) — zero dodatkowej pracy konwersji modelu.
3. Jest to spójne z już zaakceptowanym wzorcem w projekcie: `ort` (ONNX Runtime, silnik CPU-baseline ustalony w bazowej architekturze v4.5 §3.7/§18 i odziedziczony bez zmian do v6.0; ADR-49 zmienił wyłącznie *model*, nie silnik inferencji) też jest bindingiem Rust do biblioteki C++. FFI do dojrzałego silnika inferencji nie jest nowym rodzajem kompromisu — to kontynuacja tego samego wzorca, tylko dla ścieżki GPU.
4. **`CubeCL` i `krnl` zostają zapisane jako nazwane, zarezerwowane opcje na przyszłość**, nie jako coś w zakresie v6.0. Nie ma dziś zidentyfikowanej potrzeby obliczeniowej w SmartFS, której `ggml`/Vulkan by nie pokrywał. Zarezerwowane na dwa przyszłe scenariusze: (a) pojawi się potrzeba obliczeniowa poza tym, co robi `ggml` (np. coś specyficznego dla warstwy konsolidacji, gdyby kiedyś przestała mieścić się na CPU); (b) decyzja edukacyjna — chęć napisania własnych kerneli dla głębszego zrozumienia matematyki modeli, niezależnie od wymogów produkcyjnych.
5. CUDA i ROCm/HIP pozostają całkowicie wykluczone z tych samych powodów zasadowych co w rewizji 1 — żaden zamknięty, jednowendorowy stack obliczeniowy. Ta część decyzji się nie zmienia.
6. Warstwa konsolidacji semantycznej ([docs/03](../03-consolidation-design.md)) pozostaje wyłącznie na CPU — bez zmian.
7. `koval.toml`: `gpu_acceleration = "vulkan" | "cpu"` — bez zmian nazwy pola względem rewizji 1; `"vulkan"` oznacza teraz konkretnie "`ggml` zbudowany z backendem Vulkan", nie `CubeCL`.

## Odrzucone alternatywy

**`CubeCL` jako fundament już teraz.** Odrzucone przez samą zasadę reuse-before-rewrite: po co pisać matmul/attention/RMSNorm od zera i utrzymywać ten kod, skoro `ggml` już to ma, przetestowane na produkcji przez znacznie szerszą społeczność, i wspiera dokładnie nasz model. Pozostaje zarezerwowane, patrz Decyzja §4.

**`krnl` jako fundament już teraz.** To samo uzasadnienie — dodatkowo wymagałoby napisania prymitywów ML od zera w niskopoziomowym DSL, bez żadnych gotowych bloków.

**CUDA i ROCm/HIP.** Odrzucone z powodów zasadowych opisanych w Kontekście (niezmiennie od rewizji 1) — nie jako kwestia dostępności sprzętu, tylko trwałej rezygnacji z zamkniętych stacków wendorowych.

**`DirectML`.** Cross-vendor, ale Windows-only — nieistotne dla SmartFS jako projektu FUSE/Linux-first.

**Ślepe poleganie na "Vulkan wszędzie" bez zmierzenia realnych liczb.** Odrzucone — dane z sekcji "Zbadane fakty wydajnościowe" pokazują, że bez tego pomiaru decyzja byłaby ideologiczna, nie inżynierska; koszt (AMD desktop) i realne ryzyko regresji (telefony) muszą być udokumentowane, żeby `gpu_acceleration=cpu` jako domyślna wartość na słabym sprzęcie było świadomym wyborem, nie przeoczeniem.

## Otwarte pytanie

Czy warto docelowo ujednolicić CPU i GPU pod jeden silnik (`ggml` dla obu, zamiast ONNX Runtime na CPU + `ggml` na GPU), zamiast utrzymywać dwa silniki inferencji jednocześnie — nierozstrzygnięte tutaj. ADR-49 już ustalił ONNX Runtime jako fundament CPU-baseline; zmiana tego wykracza poza zakres tego ADR (dotyczącego wyłącznie ścieżki GPU). Warte osobnego ADR, jeśli utrzymanie dwóch silników okaże się w praktyce uciążliwe.

## Konsekwencje

- **Zakres pracy w v6.0 jest mniejszy niż zakładała rewizja 1 tego ADR:** nie trzeba pisać forward passu `Qwen3-Embedding` w `CubeCL` od zera. `smartfs-ai` dostaje bindingi FFI do `ggml`/`llama.cpp` zbudowanego z Vulkan zamiast CUDA/HIP — analogicznie do już istniejącej integracji z ONNX Runtime.
- Dokumentacja instalacyjna musi jasno powiedzieć: (a) na desktopowym AMD/NVIDIA Vulkan będzie wolniejszy niż odpowiednio ROCm/CUDA — świadoma cena zasady, nie błąd; (b) na telefonach ścieżka GPU może dziś być **wolniejsza niż CPU** z powodu nierozwiązanego problemu w `llama.cpp` (`#9464`) — `gpu_acceleration=cpu` pozostaje zalecaną, bezpieczną wartością na takim sprzęcie, dopóki upstream tego nie naprawi.
- `smartfs-ai` musi wykrywać i jawnie logować, z którego silnika (`ort`/CPU czy `ggml`/Vulkan) faktycznie korzysta — bez zmian względem rewizji 1.
- Minimalne wymagania sprzętowe projektu pozostają: CPU + tyle RAM, ile wymaga wybrany model embeddingowy (ADR-49) — żadnej karty graficznej, żadnego konkretnego wendora.

## Rozstrzygnięcie Otwartego pytania (2026-09-14)

Pytanie „czy warto docelowo ujednolicić CPU i GPU pod jeden silnik (`ggml` dla obu, zamiast ONNX Runtime na CPU + `ggml` na GPU)" rozstrzygnięte w [ADR-63](ADR-63-embedding-model-placement.md) §1 na **tak, i to teraz**.

Powód, dla którego moment jest właściwy: ścieżka CPU nie istniała jeszcze w kodzie. `smartfs-ai` nie miał zależności `ort`, a `CpuEmbeddingEngine` nie ładował żadnego modelu — nie było więc czego migrować. Odkładanie decyzji oznaczałoby napisanie ładowania modelu, tokenizacji i poolingu pod ONNX, żeby za jakiś czas napisać to drugi raz pod `ggml`.

Skutek dla tego ADR: `gpu_acceleration = "vulkan" | "cpu"` przestaje być przełącznikiem *między silnikami* i staje się flagą jednego silnika — czyli znaczy dokładnie to, co jego nazwa obiecywała.

### Uzupełnienie po rewizji 2 ADR-63

Trzy rzeczy, które doprecyzowują ten ADR, a wyszły dopiero przy liczeniu budżetu VRAM:

1. **„Backend Vulkan" w `ggml` to zestaw kerneli compute w SPIR-V** — shadery GLSL kompilowane do SPIR-V i uruchamiane przez `vkCmdDispatch`, bez potoku graficznego. Warto to nazwać wprost, bo pytanie „a może napisać kernele Vulkan?" ma odpowiedź „one już są", a nie „to inna droga". Zbieżnie z [ADR-60](ADR-60-plugin-architecture-rust-spirv.md), który dopuszcza w projekcie dokładnie Rust i SPIR-V.

2. **Mały VRAM nie wyklucza akceleracji.** `-ngl N` offloaduje N warstw, reszta zostaje na CPU, a w embeddingach długość kontekstu wybieramy sami, więc cache KV jest pokrętłem, nie wyrokiem. Zmierzone z pliku: warstwa Q8_0 to 15,9 MiB, cache KV 112 KiB na token — **karta z 1 GB VRAM mieści cały ten model (28 z 28 warstw) z zapasem**. Pełna tabela progów w [ADR-63](ADR-63-embedding-model-placement.md) §1e.

3. **`llvmpipe` wygląda w Vulkanie jak urządzenie i nie jest kartą.** Wykrywanie musi odrzucać `PHYSICAL_DEVICE_TYPE_CPU`, inaczej maszyna bez GPU zamelduje akcelerację i będzie liczyć wolniej niż backend CPU. Na maszynie testowej `vulkaninfo` wylicza je jako GPU1 obok prawdziwej karty — to nie jest przypadek hipotetyczny.

Konsekwencja dla zapisanej tu zasady „GPU jest bonusem, nigdy wymogiem": zostaje bez zmian, ale zyskuje mechanizm. O tym, czy bonus się opłaca, rozstrzyga pomiar per maszyna (`smartfs-worker --calibrate`, ADR-63 §1c), nie reguła — dokładnie dlatego, że zebrane wyżej liczby pokazują sprzęt, na którym GPU jest wolniejsze niż CPU.
