package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"github.com/sealbro/go-discord-caller/internal/radio"
)

func main() {
	addr := flag.String("addr", "127.0.0.1:17777", "listen address")
	guild := flag.String("guild", "dev-guild", "test guild id")
	user := flag.String("user", "dev-user", "test Discord user id")
	flag.Parse()

	reg := radio.NewRegistry()
	code, err := reg.IssuePairCode(*guild, *user)
	if err != nil {
		panic(err)
	}

	fmt.Println("LLB Command Radio DEV server")
	fmt.Println("Pair code:", code)
	fmt.Println("API:", "http://"+*addr)
	fmt.Println("Use this only for local helper/UI testing; Discord audio is not attached.")

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := (&radio.HTTPServer{Registry: reg, Addr: *addr}).Run(ctx); err != nil {
		panic(err)
	}
}
