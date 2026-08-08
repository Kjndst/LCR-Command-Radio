package main

import (
	"log"
	"os"
)

var defaultServerURL = "http://127.0.0.1:17777"
var buildVersion = "0.2.1-dev"

func main() {
	if err := run(os.Args[1:]); err != nil {
		log.Printf("LLB Command Radio: %v", err)
		os.Exit(1)
	}
}
