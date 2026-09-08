package appakagentadk

import (
	"context"
	"io"
	"net/http"
	"strings"
	"testing"
)

type captureMCPHeaders struct{ header http.Header }

func (capture *captureMCPHeaders) RoundTrip(request *http.Request) (*http.Response, error) {
	capture.header = request.Header.Clone()
	return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader("")), Header: make(http.Header)}, nil
}

func TestMCPTransportKeepsStaticHeaderPrecedenceAndOriginalRequest(t *testing.T) {
	capture := &captureMCPHeaders{}
	transport := &headerRoundTripper{
		base:    capture,
		headers: map[string]string{"Authorization": "static-token"},
		headerProvider: func(context.Context) map[string]string {
			return map[string]string{"Authorization": "sts-token", "X-Tenant": "tenant"}
		},
	}
	request, err := http.NewRequest("POST", "https://example.invalid/mcp", nil)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Authorization", "original-token")
	response, err := transport.RoundTrip(request)
	if err != nil {
		t.Fatal(err)
	}
	response.Body.Close()
	if capture.header.Get("Authorization") != "static-token" || capture.header.Get("X-Tenant") != "tenant" {
		t.Fatal(capture.header)
	}
	if request.Header.Get("Authorization") != "original-token" || request.Header.Get("X-Tenant") != "" {
		t.Fatal("mutated input request")
	}
}

func TestMCPTransportRefusesUnavailableConfiguredCA(t *testing.T) {
	path := t.TempDir() + "/missing-ca.pem"
	_, err := createTransport(context.Background(), mcpServerParams{URL: "https://example.invalid/mcp", TLSCACertPath: &path})
	if err == nil {
		t.Fatal("configured CA failure silently used default trust")
	}
}
