# Incident — CodeScribe.app 0.10.0 segfault during NSApp run loop

**Date/Time:** 2026-05-11 19:29:49.4439 -0700
**Launch Time:** 2026-05-11 19:06:17.1671 -0700 (uptime ~23.5 min before crash)
**App Version:** 0.10.0 (build 0.10.0)
**Bundle ID:** `com.codescribe.app` (Code Signing Team `MW223P3NPX`)
**OS:** macOS 26.5 (25F71), Mac15,9, ARM-64 native
**Reporter:** Maciej (operator), pasted via clipboard (overlay added "Translated Report" header — separate bug)
**Crash Reporter Key:** E92AAD47-6225-FA80-D24C-25C23E4BC4EA
**Incident ID:** 4F08C270-3967-40EF-8B23-844649123DC9

## Crash signature

| Field | Value |
|-------|-------|
| Exception Type | `EXC_BAD_ACCESS (SIGSEGV)` |
| Exception Subtype | `KERN_INVALID_ADDRESS at 0x0000145a342d9fe8` |
| Termination | `Namespace SIGNAL, Code 11, Segmentation fault: 11`, by exc handler |
| Faulting thread | Thread 0, name `main`, queue `com.apple.main-thread` |
| `pc` | `0x00000001806e0014` (`objc_release` + 16) |
| `far` | `0x0000145a342d9fe8` (Data Abort, byte read translation fault) |
| VM region | `0x145a342d9fe8 is not in any region` (UNUSED SPACE — pointer to deallocated/unmapped memory) |

## Top of crashing stack

```
0   libobjc.A.dylib        objc_release + 16
1   libobjc.A.dylib        AutoreleasePoolPage::releaseUntil(objc_object**) + 204
2   libobjc.A.dylib        objc_autoreleasePoolPop + 244
3   CoreFoundation         _CFAutoreleasePoolPop + 32
4   Foundation             -[NSAutoreleasePool drain] + 136
5   AppKit                 -[NSApplication run] + 416
6   codescribe             0x104955644 (+1447492)
7   codescribe             0x104955530 (+1447216)
8   codescribe             0x10495544c (+1446988)
9   codescribe             0x1049542f4 (+1442548)
10  codescribe             0x1048043bc (+66492)
11  codescribe             0x1047ff9d8 (+47576)
12  codescribe             0x104811d94 (+122260)
13  codescribe             0x104819cb8 (+154808)
14  dyld                   start + 6992
```

## Forensic reading

Klasyczny **use-after-release w obsłudze AppKit autorelease poola**. Crash leci w
momencie kiedy `NSApplication run` próbuje zdrenować pool (typowo na koniec event
loop iteration / pull-event scope). `objc_release` dostaje do release'a obiekt
którego pamięć została już zdealokowana — `far` wskazuje na adres w *UNUSED SPACE*
poza wszystkimi VM regionami procesu.

### Ślad w rejestrach (Thread 0, krytyczne)

```
x22: 0x00000000a1a1a1a1   ← klasyczna ObjC poison sentinel dla zwolnionego obiektu
x24: 0xa3a3a3a3a3a3a3a3   ← druga poison sentinel  
x28: 0x00000001ea316000 → OBJC_IVAR_$_NSScrubberChangeTransition._view
```

`0xa1a1a1a1` i `0xa3a3a3a3a3a3a3a3` to dobrze znane debug-fill patterns
(`SCRIBBLE_DEALLOC_PATTERN`-class) — pamięć po `dealloc`. Obiekt został zwolniony,
referencja przeszła do autorelease pool, drain próbuje go release'nąć drugi raz.

### Hipoteza root cause

**`NSScrubberChangeTransition._view` lub powiązany view** — najprawdopodobniej
Rust→ObjC bridge (codescribe ma `tokio-rt-worker` thready, czyli runtime Rusta z
ObjC frontendem) over-release'uje wrapper do widoku NSScrubber. Możliwe ścieżki:

1. **Rust drop wywołuje `release` na obiekcie który już jest w autorelease pool**
   bez zrobienia `retain` najpierw → drain potem releasuje już-zwolniony pointer.
2. **Bridge wraper (np. `objc2`/`cocoa-rs`-style)** ma race między tokio worker
   threadem a main threadem — worker wywołuje release na obiekcie którego main
   thread właśnie drainuje.
3. **NSScrubber view delegate** trzymany jako weak/unowned reference w Rust state,
   bez przedłużonego lifetime przez retain — view dealokowany przed scrubber
   change transition completion.

Crash jest `0x00000001 (kCFErrorDomainCFNetworkResolutionTimeout-style code 1)` w
exception codes — single byte read na unmapped page. Klasyczny dangling `id`.

### Powtarzalność

Nieznana z jednego crash dumpa. Operator zgłosił "ojej, kolejny CodeScribe jebnięty"
co sugeruje że to NIE jest pierwsza taka instancja — pattern. Trzeba zebrać więcej
crash reportów żeby zobaczyć czy zawsze ten sam stack (NSApp run drain), zawsze
ten sam IVAR (NSScrubberChangeTransition._view), czy losowo różne.

## Co zrobić — propozycje (po stronie CodeScribe dev)

1. **Włączyć Zombies w Debug** (`NSZombieEnabled=YES` lub `MallocStackLogging=1`)
   na test-builds — następny crash poda nazwę zombie obiektu zamiast generic
   `objc_release`.
2. **Audit każdego `release()` w Rust→ObjC bridge** który dotyka NSScrubber lub
   przyległych views. Sprawdzić czy retain count balansuje się wokół autorelease.
3. **Włączyć ARC enforcement na stronie ObjC** (jeśli MRR jest gdziekolwiek) — w
   dzisiejszym AppKitcie pure-MRR powinno być eradykowane.
4. **Sprawdzić tokio worker thread interactions z main thread** — żaden ObjC
   release nie powinien lecieć z worker threada bez `dispatch_async(main)`.
5. **Crash reporting integration** (Sentry/Crashlytics) — żeby Maciej nie musiał
   ręcznie pastować raportów przez clipboard, który wstrzykuje overlay garbage.

## Powiązane bugi w samym overlay paste flow

- **Garbage prefix:** clipboard paste wstrzykuje header
  `------------------------------------- / Translated Report (Full Report Below) / -------------------------------------`
  przed content. To jest *paste flow bug*, nie część crasha. Tracked osobno w
  `BACKLOG.md`.
- **`plik` → `blik`:** overlay autocorrect/transcription mangluje polskie słowo.
  Tracked osobno w `BACKLOG.md`.

## Pełny crash report

Pełen Apple crash report (~100 KB plain text) pasted by operator do tej sesji.
Nie zapisany do disku w całości (size + clipboard origin). Forensikę powyżej
zsyntetyzowano z sekcji: `Exception Type`, `Triggered by Thread`, `Thread 0 Crashed`
ARM thread state, `Binary Images`, `vmregioninfo`, `legacyInfo`. Jeśli
deweloperzy CodeScribe potrzebują pełnego reportu — operator ma w bufferze (lub
w macOS DiagnosticReports `~/Library/Logs/DiagnosticReports/codescribe-*.ips`).

---

*𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by VetCoders (c)2024-2026 The LibraxisAI Team*
