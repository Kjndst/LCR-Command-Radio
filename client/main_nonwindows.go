//go:build !windows

package main

import "fmt"

func run(args []string) error {
	return fmt.Errorf("LLB Command Radio client is Windows-only")
}
