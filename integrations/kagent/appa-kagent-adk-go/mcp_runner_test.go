package appakagentadk

import (
	"reflect"
	"testing"

	"github.com/kagent-dev/kagent/go/api/adk"
)

func TestDiscoveryAssemblyIsolatesEveryDelegationWithoutChangingSourceConfig(t *testing.T) {
	config := &adk.AgentConfig{
		RemoteAgents: []adk.RemoteAgentConfig{
			{Name: "analyst", Url: "http://analyst:8080", Headers: map[string]string{"x-example": "value"}},
			{Name: "reviewer", Url: "http://reviewer:8080", IsolateSessions: true},
		},
		HttpTools: []adk.HttpMcpServerConfig{{}},
		SseTools:  []adk.SseMcpServerConfig{{}},
	}
	before := append([]adk.RemoteAgentConfig(nil), config.RemoteAgents...)
	assembled := discoveryAgentConfig(config)
	if !reflect.DeepEqual(config.RemoteAgents, before) || len(config.HttpTools) != 1 || len(config.SseTools) != 1 {
		t.Fatal("APPA assembly modified the source config used by the stock path")
	}
	if assembled.HttpTools != nil || assembled.SseTools != nil {
		t.Fatal("MCP tools must remain owned by invocation discovery")
	}
	for i, remote := range assembled.RemoteAgents {
		if !remote.IsolateSessions {
			t.Fatalf("delegation %d still shares a child session", i)
		}
		want := before[i]
		want.IsolateSessions = true
		if !reflect.DeepEqual(remote, want) {
			t.Fatalf("delegation %d lost its original settings", i)
		}
	}
}
