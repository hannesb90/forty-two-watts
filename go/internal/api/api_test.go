package api

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestParseRangeSupports48h(t *testing.T) {
	const want = 48 * 60 * 60 * 1000
	if got := parseRange("48h"); got != want {
		t.Fatalf("parseRange(48h) = %d, want %d", got, want)
	}
}

// handleEVCommand rejects anything not in the allowlist with 400. The Lua
// driver's command hook silently returns nil for unknown actions, so
// without this gate the API would 200-OK typos.
func TestHandleEVCommandRejectsUnknownActions(t *testing.T) {
	srv := New(&Deps{}) // registry/tel unset — the 400 branches run first

	cases := []struct {
		name string
		body string
		want int
	}{
		{"unknown action", `{"action":"ev_nuke"}`, http.StatusBadRequest},
		{"empty action", `{"action":""}`, http.StatusBadRequest},
		{"missing action field", `{}`, http.StatusBadRequest},
		{"malformed json", `{`, http.StatusBadRequest},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			req := httptest.NewRequest(http.MethodPost, "/api/ev/command", strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			rr := httptest.NewRecorder()
			srv.Handler().ServeHTTP(rr, req)
			if rr.Code != tc.want {
				t.Fatalf("body=%s: got status %d, want %d (body: %s)", tc.body, rr.Code, tc.want, rr.Body.String())
			}
		})
	}
}

// Allowlisted actions pass the action-validation gate. With a nil
// Registry they then short-circuit to 503, which confirms they got past
// the 400 branch without us needing a real driver registry.
func TestHandleEVCommandAcceptsAllowlistedActions(t *testing.T) {
	srv := New(&Deps{})

	for action := range validEVActions {
		t.Run(action, func(t *testing.T) {
			body := `{"action":"` + action + `"}`
			req := httptest.NewRequest(http.MethodPost, "/api/ev/command", strings.NewReader(body))
			req.Header.Set("Content-Type", "application/json")
			rr := httptest.NewRecorder()
			srv.Handler().ServeHTTP(rr, req)
			if rr.Code != http.StatusServiceUnavailable {
				t.Fatalf("action=%q: got status %d, want 503 (body: %s)", action, rr.Code, rr.Body.String())
			}
		})
	}
}
