package drivers

import (
	"context"
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"testing"
	"time"

	"github.com/simonvetter/modbus"

	"github.com/frahlg/forty-two-watts/go/cmd/sim-sungrow/sungrow"
	"github.com/frahlg/forty-two-watts/go/internal/telemetry"
)

// findSungrowWASM walks up to find drivers-wasm/sungrow.wasm.
func findSungrowWASM(t *testing.T) string {
	t.Helper()
	_, file, _, _ := runtime.Caller(0)
	dir := filepath.Dir(file)
	for i := 0; i < 6; i++ {
		candidate := filepath.Join(dir, "drivers-wasm", "sungrow.wasm")
		if _, err := filepathStat(candidate); err == nil {
			return candidate
		}
		dir = filepath.Dir(dir)
	}
	t.Skip("drivers-wasm/sungrow.wasm not found — run `make wasm` first")
	return ""
}

// modbusTCPCap adapts a simonvetter/modbus client to the drivers.ModbusCap interface.
type modbusTCPCap struct {
	mu  sync.Mutex
	cli *modbus.ModbusClient
}

func newModbusCap(t *testing.T, port string) *modbusTCPCap {
	t.Helper()
	cli, err := modbus.NewClient(&modbus.ClientConfiguration{
		URL:     "tcp://127.0.0.1:" + port,
		Timeout: 2 * time.Second,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := cli.Open(); err != nil {
		t.Fatal(err)
	}
	return &modbusTCPCap{cli: cli}
}

func (m *modbusTCPCap) Read(addr, count uint16, kind int32) ([]uint16, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	var rt modbus.RegType
	switch kind {
	case ModbusInput:
		rt = modbus.INPUT_REGISTER
	case ModbusHolding:
		rt = modbus.HOLDING_REGISTER
	default:
		rt = modbus.INPUT_REGISTER
	}
	return m.cli.ReadRegisters(addr, count, rt)
}

func (m *modbusTCPCap) WriteSingle(addr, value uint16) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.cli.WriteRegister(addr, value)
}

func (m *modbusTCPCap) WriteMulti(addr uint16, values []uint16) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.cli.WriteRegisters(addr, values)
}

// TestIntegration_SungrowWasmDriverAgainstSimulator is the end-to-end proof
// that the Go host + WASM driver + Modbus simulator stack works for Sungrow.
func TestIntegration_SungrowWasmDriverAgainstSimulator(t *testing.T) {
	wasmPath := findSungrowWASM(t)

	// 1. Start Sungrow physics simulator + Modbus TCP server
	port := pickFreePort(t)
	cfg := sungrow.Default()
	cfg.ResponseTauS = 0.2
	cfg.PVPeakW = 1500 // constant so we can verify
	cfg.SoC = 0.6
	sim := sungrow.New(cfg)
	bank := sungrow.NewRegisterBank(sim)
	bank.Refresh(sim.Tick(time.Millisecond))

	srv, err := modbus.NewServer(&modbus.ServerConfiguration{
		URL:        "tcp://127.0.0.1:" + port,
		Timeout:    5 * time.Second,
		MaxClients: 4,
	}, &sungrowTestHandler{bank: bank})
	if err != nil {
		t.Fatal(err)
	}
	if err := srv.Start(); err != nil {
		t.Fatal(err)
	}
	defer srv.Stop()

	// Physics ticker
	stop := make(chan struct{})
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		tk := time.NewTicker(100 * time.Millisecond)
		defer tk.Stop()
		last := time.Now()
		for {
			select {
			case <-stop: return
			case now := <-tk.C:
				dt := now.Sub(last); last = now
				bank.Refresh(sim.Tick(dt))
			}
		}
	}()
	defer func() { close(stop); wg.Wait() }()
	time.Sleep(150 * time.Millisecond)

	// 2. Load WASM driver with Modbus capability
	ctx := context.Background()
	rt := NewRuntime(ctx)
	defer rt.Close(ctx)

	tel := telemetry.NewStore()
	env := NewHostEnv("sungrow", tel).WithModbus(newModbusCap(t, port))

	drv, err := rt.Load(ctx, wasmPath, env)
	if err != nil {
		t.Fatal(err)
	}
	defer drv.Cleanup(ctx)

	// 3. Init the driver — should read SN + device type via Modbus
	if err := drv.Init(ctx, []byte(`{}`)); err != nil {
		t.Fatalf("driver_init: %v", err)
	}
	make, sn := env.Identity()
	if make != "Sungrow" {
		t.Errorf("make: got %q", make)
	}
	if sn == "" {
		t.Error("serial number not set by driver")
	}
	t.Logf("identity: make=%q sn=%q", make, sn)

	// 4. Poll a few times, let telemetry flow
	for i := 0; i < 5; i++ {
		if _, err := drv.Poll(ctx); err != nil {
			t.Fatalf("poll: %v", err)
		}
		time.Sleep(150 * time.Millisecond)
	}

	// 5. Verify site-convention signs in telemetry
	meter := tel.Get("sungrow", telemetry.DerMeter)
	pv := tel.Get("sungrow", telemetry.DerPV)
	bat := tel.Get("sungrow", telemetry.DerBattery)

	if meter == nil { t.Fatal("no meter reading") }
	if pv == nil { t.Fatal("no pv reading") }
	if bat == nil { t.Fatal("no battery reading") }

	t.Logf("meter: w=%.1f hz=%s", meter.RawW, meter.Data)
	t.Logf("pv:    w=%.1f (site: + = source)", pv.RawW)
	if bat.SoC != nil {
		t.Logf("bat:   w=%.1f SoC=%.3f", bat.RawW, *bat.SoC)
	}

	// Site convention (boundary view): PV pushes TO site → NEGATIVE.
	// Sim uses PVPeakW = 1500W of generation.
	if pv.RawW > -1000 || pv.RawW < -2000 {
		t.Errorf("pv should be ~-1500W (site: generation is negative), got %.1f", pv.RawW)
	}

	// SoC should roundtrip
	if bat.SoC == nil {
		t.Fatal("SoC not populated")
	}
	if *bat.SoC < 0.55 || *bat.SoC > 0.65 {
		t.Errorf("SoC expected ~0.6, got %.3f", *bat.SoC)
	}

	// 6. Issue a DISCHARGE command. Site convention (boundary view):
	//    power_w < 0 = discharge (source, reduces site import).
	if err := drv.Command(ctx, []byte(`{"action":"battery","power_w":-2000}`)); err != nil {
		t.Fatalf("command: %v", err)
	}
	for i := 0; i < 20; i++ {
		_, _ = drv.Poll(ctx)
		time.Sleep(150 * time.Millisecond)
	}
	bat = tel.Get("sungrow", telemetry.DerBattery)
	t.Logf("after -2000W discharge command: bat.w=%.1f (site: − = discharge)", bat.RawW)
	// Expect ~-2000*0.96 = -1920W (negative in site convention)
	if bat.RawW > -1000 {
		t.Errorf("expected negative discharge (~-1900W), got %.1f", bat.RawW)
	}

	// 7. Now CHARGE command. Site convention: power_w > 0 = charge (load).
	if err := drv.Command(ctx, []byte(`{"action":"battery","power_w":1000}`)); err != nil {
		t.Fatalf("command: %v", err)
	}
	for i := 0; i < 20; i++ {
		_, _ = drv.Poll(ctx)
		time.Sleep(150 * time.Millisecond)
	}
	bat = tel.Get("sungrow", telemetry.DerBattery)
	t.Logf("after +1000W charge command: bat.w=%.1f (site: + = charge)", bat.RawW)
	// Expect ~+1000*0.96 = +960W
	if bat.RawW < 500 {
		t.Errorf("expected positive charge (~+960W), got %.1f", bat.RawW)
	}

	// 8. Default mode — revert to stop
	if err := drv.DefaultMode(ctx); err != nil {
		t.Fatalf("default_mode: %v", err)
	}
	for i := 0; i < 15; i++ {
		_, _ = drv.Poll(ctx)
		time.Sleep(100 * time.Millisecond)
	}
	bat = tel.Get("sungrow", telemetry.DerBattery)
	t.Logf("after default_mode: bat.w=%.1f (should relax toward 0)", bat.RawW)
	// Should relax toward zero (might still have some residual)
	if absFloat(bat.RawW) > 500 {
		t.Errorf("expected battery to relax after default_mode, still at %.1f", bat.RawW)
	}

	// Health
	h := tel.DriverHealth("sungrow")
	if h == nil || h.Status != telemetry.StatusOk {
		t.Errorf("driver health: %+v", h)
	}
	t.Logf("final health: %s (ticks=%d)", h.Status, h.TickCount)
}

// ---- test helpers ----

func absFloat(f float64) float64 {
	if f < 0 { return -f }
	return f
}

// filepathStat wraps os.Stat so we can use it without an extra helper file.
func filepathStat(path string) (os.FileInfo, error) {
	return os.Stat(path)
}

// ---- Modbus handler ----

type sungrowTestHandler struct{ bank *sungrow.RegisterBank }

func (h *sungrowTestHandler) HandleCoils(_ *modbus.CoilsRequest) ([]bool, error) {
	return nil, modbus.ErrIllegalFunction
}
func (h *sungrowTestHandler) HandleDiscreteInputs(_ *modbus.DiscreteInputsRequest) ([]bool, error) {
	return nil, modbus.ErrIllegalFunction
}
func (h *sungrowTestHandler) HandleHoldingRegisters(req *modbus.HoldingRegistersRequest) ([]uint16, error) {
	if req.IsWrite {
		return nil, h.bank.WriteHolding(req.Addr, req.Args)
	}
	return h.bank.ReadHolding(req.Addr, req.Quantity), nil
}
func (h *sungrowTestHandler) HandleInputRegisters(req *modbus.InputRegistersRequest) ([]uint16, error) {
	return h.bank.ReadInput(req.Addr, req.Quantity), nil
}
