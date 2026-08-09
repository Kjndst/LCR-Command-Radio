package main

import (
	"log"
	"os"
)

var defaultServerURL = "https://lcr.tail7b8791.ts.net"
var buildVersion = "0.0.1 Beta"

func main() {
	if err := run(os.Args[1:]); err != nil {
		log.Printf("LLB Command Radio: %v", err)
		os.Exit(1)
	}
}
