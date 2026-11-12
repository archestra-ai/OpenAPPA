// Command appa-extproc enforces OpenAPPA at Agent Substrate's egress gateway.
//
// Substrate sends every request an actor makes through its egress gateway, an
// Envoy that terminates TLS for the hosts an EgressPolicy names. With an
// additional ext_proc service configured, the gateway calls this processor for
// each request and passes the actor's verified SPIFFE ID with it. For a host
// configured as an MCP server, the processor reads each JSON-RPC `tools/call`,
// asks an OpenAPPA runtime whether the call may run, and either lets it through
// or answers it with a tool error before it leaves the cluster. It then reports
// the result to the runtime, which labels the actor's trajectory.
//
// Nothing runs inside the sandbox, so the agent, its harness, and its image
// play no part: any MCP client the actor runs is checked the same way.
package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"flag"
	"fmt"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	extprocv3 "github.com/envoyproxy/go-control-plane/envoy/service/ext_proc/v3"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
)

func main() {
	var servers serverFlag
	listen := flag.String("listen", ":9443", "gRPC address the egress gateway dials")
	adminListen := flag.String("admin-listen", "127.0.0.1:8081", "address of the decision log and reset endpoint; empty disables it")
	runtimeURL := flag.String("runtime-url", "http://127.0.0.1:8787", "OpenAPPA runtime base URL")
	certBundle := flag.String("tls-credential-bundle", "", "PEM file holding the serving key followed by its certificate chain (a pod certificate's credential bundle); reread on each handshake")
	clientCA := flag.String("tls-client-ca", "", "PEM trust bundle that client certificates must chain to; required with --tls-credential-bundle")
	clientID := flag.String("tls-client-id", "", "the URI SAN a client certificate must carry, e.g. the egress gateway's spiffe://cluster.local/ns/ate-system/sa/<name>; empty accepts any certificate the trust bundle verifies")
	plaintext := flag.Bool("plaintext", false, "serve gRPC without TLS (tests only)")
	acceptNarrowing := flag.Bool("accept-narrowing", true, "take a remedy that only narrows the actor's label and needs no review, then run the call")
	flag.Var(&servers, "mcp-server", "HOST=NAME: requests to HOST are MCP calls to server NAME, spelled mcp:NAME/<tool>; repeatable")
	flag.Parse()

	if len(servers) == 0 {
		log.Fatal("appa-extproc: name at least one --mcp-server HOST=NAME")
	}
	if !*plaintext && (*certBundle == "" || *clientCA == "") {
		log.Fatal("appa-extproc: set --tls-credential-bundle and --tls-client-ca, or --plaintext")
	}

	runtime := newRuntimeClient(*runtimeURL)
	journal := newJournal(200)
	processor := newProcessor(processorConfig{
		servers:         servers,
		runtime:         runtime,
		journal:         journal,
		acceptNarrowing: *acceptNarrowing,
	})

	var options []grpc.ServerOption
	if !*plaintext {
		creds, err := serverCredentials(*certBundle, *clientCA, *clientID)
		if err != nil {
			log.Fatalf("appa-extproc: %v", err)
		}
		options = append(options, grpc.Creds(creds))
	}
	server := grpc.NewServer(options...)
	extprocv3.RegisterExternalProcessorServer(server, processor)

	listener, err := net.Listen("tcp", *listen)
	if err != nil {
		log.Fatalf("appa-extproc: %v", err)
	}
	go func() {
		log.Printf("appa-extproc: serving ext_proc on %s for %s", *listen, servers)
		if err := server.Serve(listener); err != nil {
			log.Fatalf("appa-extproc: %v", err)
		}
	}()

	var admin *http.Server
	if *adminListen != "" {
		admin = &http.Server{Addr: *adminListen, Handler: adminHandler(processor, journal, runtime), ReadHeaderTimeout: 5 * time.Second}
		go func() {
			log.Printf("appa-extproc: decision log on http://%s/", *adminListen)
			if err := admin.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
				log.Fatalf("appa-extproc: %v", err)
			}
		}()
	}

	stop := make(chan os.Signal, 1)
	signal.Notify(stop, syscall.SIGINT, syscall.SIGTERM)
	<-stop
	server.GracefulStop()
	if admin != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = admin.Shutdown(ctx)
	}
}

// serverFlag maps a request host to the MCP server name its tools carry.
type serverFlag map[string]string

func (s *serverFlag) String() string {
	if s == nil {
		return ""
	}
	parts := make([]string, 0, len(*s))
	for host, name := range *s {
		parts = append(parts, host+"="+name)
	}
	return strings.Join(parts, ",")
}

func (s *serverFlag) Set(value string) error {
	host, name, ok := strings.Cut(value, "=")
	if !ok || host == "" || !validSegment(name) {
		return fmt.Errorf("want HOST=NAME with NAME in [A-Za-z0-9_.-], got %q", value)
	}
	if *s == nil {
		*s = serverFlag{}
	}
	(*s)[strings.ToLower(host)] = name
	return nil
}

// serverCredentials serves the pod certificate and requires the gateway's.
// The egress gateway presents its pod-identity certificate and checks this
// server's against the service-DNS trust bundle, so both files come from
// Substrate's pod certificate signers and the kubelet rotates them.
func serverCredentials(bundlePath, clientCAPath, clientID string) (credentials.TransportCredentials, error) {
	if _, err := clientPool(clientCAPath); err != nil {
		return nil, err
	}
	base := &tls.Config{
		MinVersion: tls.VersionTLS13,
		ClientAuth: tls.RequireAndVerifyClientCert,
		NextProtos: []string{"h2"},
		// The kubelet rotates both files, so each handshake reads them.
		GetConfigForClient: func(*tls.ClientHelloInfo) (*tls.Config, error) {
			pool, err := clientPool(clientCAPath)
			if err != nil {
				return nil, err
			}
			bundle, err := os.ReadFile(bundlePath)
			if err != nil {
				return nil, err
			}
			certificate, err := tls.X509KeyPair(bundle, bundle)
			if err != nil {
				return nil, err
			}
			return &tls.Config{
				MinVersion:   tls.VersionTLS13,
				ClientAuth:   tls.RequireAndVerifyClientCert,
				ClientCAs:    pool,
				Certificates: []tls.Certificate{certificate},
				NextProtos:   []string{"h2"},
				// Every pod in the cluster can hold a pod-identity
				// certificate, and a caller that reaches this processor
				// speaks for the actors it names.
				VerifyConnection: func(state tls.ConnectionState) error {
					if clientID == "" {
						return nil
					}
					if len(state.PeerCertificates) > 0 {
						for _, uri := range state.PeerCertificates[0].URIs {
							if uri.String() == clientID {
								return nil
							}
						}
					}
					return fmt.Errorf("the client certificate does not name %s", clientID)
				},
			}, nil
		},
	}
	return credentials.NewTLS(base), nil
}

func clientPool(path string) (*x509.CertPool, error) {
	caPEM, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("reading the client trust bundle: %w", err)
	}
	pool := x509.NewCertPool()
	if !pool.AppendCertsFromPEM(caPEM) {
		return nil, fmt.Errorf("%s holds no certificate", path)
	}
	return pool, nil
}

func validSegment(segment string) bool {
	if segment == "" {
		return false
	}
	for _, r := range segment {
		if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '_' || r == '.' || r == '-') {
			return false
		}
	}
	return true
}
