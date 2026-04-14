# Clamping — the reality check between math and burning wires

**Clamping** is the limiting of a value to an allowed range before it leaves
our control system and reaches the hardware. It's one of the most important
safety principles in real-time control:

```rust
fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value > max { max }
    else if value < min { min }
    else { value }
}
```

Looks trivial. But clamping is **the glue that keeps the whole system
honest**. Every nonlinear reality that the control law doesn't model has to
be caught by a clamp somewhere, or the system will eventually produce a
value that breaks something physical.

## The guiding principle

> **A clamp must protect against a *quantifiable* risk. If you can't state
> exactly what it saves you from, it's probably a bug in disguise.**

- ✅ *Fuse guard* — protects the main breaker from tripping
- ✅ *SoC floor* — protects battery cells from deep discharge below 5%
- ❌ *Saturation curve* — "maybe the battery can't do it??" — vague → became
  self-reinforcing (see case study below)

Prefer one **clear, conservative clamp** (e.g. a hard 5kW per-command cap)
over a "smart" clamp that depends on learned data that can be wrong.

## The seven clamps in forty-two-watts

| # | Clamp | Location | Protects against | Failure mode if removed |
|---|---|---|---|---|
| 1 | Per-command cap ±5000W | `control::clamp_with_soc()` | Bug or RLS divergence producing 50 000W command | Driver pushes absurd value to BMS; best case ignored, worst case PCS fault |
| 2 | SoC floor (no discharge <5%) | `control::clamp_with_soc()` | Deep discharge on already empty battery | Cell damage, BMS hard cut-off, possibly irreversible capacity loss |
| 3 | Saturation curve | `BatteryModel::clamp_to_saturation()` | Asking for power the battery can't deliver at current SoC | Command silently ignored by BMS; target/actual diverge; PI integral winds up |
| 4 | Slew rate (±500W/cycle) | `control::compute_dispatch_with_models()` | Step changes that trigger oscillation or current spikes | Grid transients, EMI, premature inverter wear, user-visible flicker |
| 5 | Fuse guard | `control::apply_fuse_guard()` | Total PV + discharge exceeding the shared breaker limit | **Main fuse blows at 03:00. House dark. Not recoverable remotely.** |
| 6 | RLS parameter bounds `a ∈ [0.1, 0.99]`, `b ∈ [-1.5, 1.5]` | `BatteryModel::update()` | Numerical divergence under pathological data | Parameter explosion; learned "gain" of 1e10; subsequent inverse model produces astronomical commands |
| 7 | Steady-state gain clamp `[0.3, 1.5]` | `BatteryModel::steady_state_gain()` | Raw `b/(1-a)` returning absurd ratios when `a → 1` | Inverse model divides by ~0, produces monster commands |

## Why clamping matters — three reasons, escalating drama

### 1. Physical safety

The fuse guard is **non-negotiable**. If batteries + PV collectively try to
push >16A through a 16A fuse, the fuse blows. Our clamp scales discharge
targets proportionally so total generation never exceeds `max_amps × voltage
× phases`. Without this clamp, a worst-case dispatch event on a sunny
afternoon takes out the whole house.

### 2. Graceful degradation

When a *model* is wrong, the system must **not rely on the model**. This is
the core idea behind the cascade-confidence gate:

```rust
if model.confidence() < 0.5 {
    // Bypass ALL model-based behaviour — clamp, inner PI, inverse.
    // Trust the raw proportional target + later safety clamps instead.
    continue;
}
```

Better to be *simpler and correct* than *clever and wrong*.

### 3. Control stability

A PI controller without anti-windup pumps up its integral term during
saturation. When the error finally reverses, the integral is monstrous —
the system overshoots, oscillates, and can take minutes to settle. Our PI
integrator clamps (`I ∈ [-3000W, 3000W]`) prevent this.

The slew rate clamp serves a related purpose: **even if the PI says
"discharge 5000W more, now"**, we only let the target move by ±500W per
cycle. The battery's internal control loop gets a smooth ramp instead of a
step, which:

- Keeps current transients below the breaker margin
- Lets the site meter catch up before we issue another change
- Avoids interaction with the battery's own internal PI loops

## Case study: the self-reinforcing saturation clamp

This is a real bug we shipped and fixed. Documented here as a cautionary
tale — not all clamps are automatically safe.

### The intent

`BatteryModel.max_discharge_curve` is a learned per-SoC envelope of what
the battery has actually delivered. If we've only ever seen Ferroamp
deliver 2kW discharge at SoC 0.5, don't ask it for 5kW — we know the BMS
will just silently reject it.

The implementation tracked maxes per 5%-SoC bucket:

```rust
// Intended behaviour
curve[bucket] = max(curve[bucket], observed_actual);
```

### The failure chain

```
Cycle N+0:   curve = []                         ← empty after model reset
             proportional wants -900W           ← no clamp applies
             command sent: -900W
             actual observed: -900W             ← battery responded normally

  ↓  transient event: battery briefly responds weakly, actual drops to -255W  ↓

Cycle N+1:   curve = [[0.5, 255]]               ← seeded from the weak actual
             proportional wants -900W
             clamp_to_saturation(-900, 0.5)     ← curve caps at -255W
             command sent: -255W
             actual: -255W                      ← because we asked for 255W
             curve update: max(255, 255) = 255  ← confirmed!

Cycle N+2:   Same as N+1. Forever.              ← LOCKED
```

The clamp and the data source formed a closed feedback loop. The curve
thought it was observing the battery's true envelope; in reality, it was
observing the clamp's own output.

### The fix

Two layers, both in `battery_model.rs`:

**Layer A — Don't seed a bucket with small values.**

```rust
const MIN_SATURATION_SEED_W: f64 = 1000.0;

fn update_one_curve(curve: &mut Vec<(f64, f64)>, bucket: f64, value: f64) {
    match curve.binary_search_by(...) {
        Ok(idx) => {
            // Existing buckets can still grow upward from any observation
            if value > curve[idx].1 { curve[idx].1 = value; }
        }
        Err(idx) => {
            // New buckets only from observations that show meaningful activity
            if value >= MIN_SATURATION_SEED_W {
                curve.insert(idx, (bucket, value));
            }
        }
    }
}
```

A small observation can no longer seed a bucket and lock it. If the
battery is being clamped to 255W, the curve stays empty — no new data
point is recorded — until we actually see ≥1kW somewhere.

**Layer B — Don't trust the curve until the model is confident.**

```rust
if model.confidence() < CASCADE_CONFIDENCE_THRESHOLD {
    continue; // skip saturation clamp + inner PI + inverse entirely
}
```

During warmup or after a reset, the curve might be empty or sparse. We
don't use it at all until enough independent high-variance samples have
built up (confidence ≥ 0.5). Until then, the proportional target passes
through directly to the slew + fuse guard — both of which are safe,
quantifiable clamps.

### Regression test

```rust
#[test]
fn saturation_curve_ignores_small_observations() {
    let mut m = BatteryModel::new("locked");
    for _ in 0..20 {
        m.update(-500.0, -255.0, 0.5, 5.0, 1000);
    }
    assert!(m.max_discharge_curve.is_empty());
}
```

This test locks in the "no self-reinforcement" behaviour. If anyone later
changes `MIN_SATURATION_SEED_W` without thinking about the feedback loop,
the test catches it.

## Rules of thumb when adding a new clamp

1. **Write down what it protects against in one sentence.** If the sentence
   is vague, either sharpen it or don't add the clamp.
2. **Prefer static clamps over learned ones.** A constant `max_amps: 16`
   from config is more robust than a curve fit from data.
3. **If the clamp's input depends on the clamp's output, watch for feedback
   loops.** The saturation-curve bug is the canonical example.
4. **Clamps must degrade gracefully.** When a clamp can't be computed
   (missing model, low confidence, empty curve), fall back to pass-through
   — never lock or refuse.
5. **Log when a clamp activates.** The `clamped: bool` field on
   `DispatchTarget` surfaces this up to the API + UI so operators can see
   when the system is being limited by a clamp.
6. **Write a regression test for the failure mode.** Bugs in clamps are
   silent — they don't crash, they just misbehave. Tests lock in the
   invariant.

## TL;DR

Clamping is the boundary between "theoretically optimal control" (which
can trip the main fuse) and "dispatch you can leave running at 3 AM".
Every clamp is a humility statement: *"my model might be wrong about this,
so here's a hard limit just in case."*

The seven clamps in this codebase were chosen to cover the seven failure
modes we know we can't model away. Adding an eighth requires the same
discipline: name the failure mode, prove the clamp catches it, write a
test that fails if it doesn't.
