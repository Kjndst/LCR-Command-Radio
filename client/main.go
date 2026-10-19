package main

import (
	"log"
	"os"
)

// No personal/private infrastructure endpoint is embedded in public source.
// Release builders must supply an explicit -X main.defaultServerURL value.
var defaultServerURL = ""
var buildVersion = "0.0.1 Beta"

func main() {
	if err := run(os.Args[1:]); err != nil {
		log.Printf("LLB Command Radio: %v", err)
		os.Exit(1)
	}
}
