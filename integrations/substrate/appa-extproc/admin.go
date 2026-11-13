// The decision log: what the processor decided, newest last, and each
// trajectory's label after the decision. It serves on loopback by default;
// it names actors and tool arguments, so expose it only to operators.
//
//	GET  /                       the log as a page
//	GET  /events?after=N         entries after sequence N, as JSON
//	GET  /status?actor=ID        the runtime's status of the actor's trajectory
//	POST /reset?actor=ID         start a new trajectory for the actor

package main

import (
	_ "embed"
	"encoding/json"
	"net/http"
	"strconv"
	"sync"
	"time"
)

//go:embed log.html
var logPage []byte

type entry struct {
	Seq       uint64          `json:"seq"`
	Time      time.Time       `json:"time"`
	Actor     string          `json:"actor"`
	Root      string          `json:"trajectory"`
	Tool      string          `json:"tool"`
	Arguments json.RawMessage `json:"arguments,omitempty"`
	Verdict   string          `json:"verdict"`
	Reason    string          `json:"reason,omitempty"`
	Status    json.RawMessage `json:"status,omitempty"`
}

type journal struct {
	mu      sync.Mutex
	limit   int
	next    uint64
	entries []entry
}

func newJournal(limit int) *journal {
	return &journal{limit: limit}
}

func (j *journal) add(e entry) {
	j.mu.Lock()
	defer j.mu.Unlock()
	j.next++
	e.Seq = j.next
	j.entries = append(j.entries, e)
	if len(j.entries) > j.limit {
		j.entries = j.entries[len(j.entries)-j.limit:]
	}
}

func (j *journal) after(seq uint64) []entry {
	j.mu.Lock()
	defer j.mu.Unlock()
	out := []entry{}
	for _, e := range j.entries {
		if e.Seq > seq {
			out = append(out, e)
		}
	}
	return out
}

func adminHandler(p *processor, j *journal, runtime *runtimeClient) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /{$}", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = w.Write(logPage)
	})
	mux.HandleFunc("GET /events", func(w http.ResponseWriter, r *http.Request) {
		after, _ := strconv.ParseUint(r.URL.Query().Get("after"), 10, 64)
		writeJSON(w, j.after(after))
	})
	mux.HandleFunc("GET /status", func(w http.ResponseWriter, r *http.Request) {
		actor := r.URL.Query().Get("actor")
		if actor == "" {
			http.Error(w, "actor is required", http.StatusBadRequest)
			return
		}
		status, err := runtime.status(r.Context(), p.trajectory(actor))
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadGateway)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write(status)
	})
	mux.HandleFunc("POST /reset", func(w http.ResponseWriter, r *http.Request) {
		actor := r.URL.Query().Get("actor")
		if actor == "" {
			http.Error(w, "actor is required", http.StatusBadRequest)
			return
		}
		writeJSON(w, map[string]string{"trajectory": p.reset(actor)})
	})
	return mux
}

func writeJSON(w http.ResponseWriter, value any) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(value)
}
