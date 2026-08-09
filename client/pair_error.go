package main

import (
	"context"
	"errors"
	"net"
	"strings"
)

const (
	pairErrorCodeInvalid = "CODE EXPIRED / INVALID"
	pairErrorUnreachable = "SERVER UNREACHABLE"
	pairErrorDNSNotReady = "SERVER DNS NOT READY"
	pairErrorUnknown     = "PAIR FAILED"
)

func classifyPairError(err error) string {
	if err == nil {
		return pairErrorUnknown
	}

	var dnsErr *net.DNSError
	if errors.As(err, &dnsErr) || hasDNSFailureText(err.Error()) {
		return pairErrorDNSNotReady
	}

	errText := strings.ToLower(err.Error())
	if strings.Contains(errText, "401") || strings.Contains(errText, "expired") || strings.Contains(errText, "invalid") {
		return pairErrorCodeInvalid
	}
	if errors.Is(err, context.DeadlineExceeded) || hasReachabilityFailureText(errText) {
		return pairErrorUnreachable
	}
	var netErr net.Error
	if errors.As(err, &netErr) && netErr.Timeout() {
		return pairErrorUnreachable
	}
	return pairErrorUnknown
}

func hasDNSFailureText(errText string) bool {
	errText = strings.ToLower(errText)
	return strings.Contains(errText, "no such host") ||
		strings.Contains(errText, "no host is known") ||
		strings.Contains(errText, "dns lookup") ||
		strings.Contains(errText, "name or service not known")
}

func hasReachabilityFailureText(errText string) bool {
	return strings.Contains(errText, "connect") ||
		strings.Contains(errText, "refused") ||
		strings.Contains(errText, "timeout") ||
		strings.Contains(errText, "deadline") ||
		strings.Contains(errText, "network is unreachable") ||
		strings.Contains(errText, "no route to host")
}
