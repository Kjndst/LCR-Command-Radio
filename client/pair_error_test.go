package main

import (
	"context"
	"errors"
	"fmt"
	"net"
	"testing"
)

func TestClassifyPairError(t *testing.T) {
	for _, tc := range []struct {
		name string
		err  error
		want string
	}{
		{"invalid code", errors.New("server returned 401 Unauthorized: invalid code"), pairErrorCodeInvalid},
		{"expired code", errors.New("pair code expired"), pairErrorCodeInvalid},
		{"connection refused", errors.New("dial tcp: connection refused"), pairErrorUnreachable},
		{"deadline", context.DeadlineExceeded, pairErrorUnreachable},
		{"typed DNS", fmt.Errorf("pair: %w", &net.DNSError{Name: "lcr.example", Err: "no such host"}), pairErrorDNSNotReady},
		{"Windows DNS", errors.New("lookup lcr.example: No such host is known"), pairErrorDNSNotReady},
		{"unknown", errors.New("unexpected response encoding"), pairErrorUnknown},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := classifyPairError(tc.err); got != tc.want {
				t.Fatalf("classifyPairError(%v) = %q, want %q", tc.err, got, tc.want)
			}
		})
	}
}
