// http_fanout.go — Go baseline for benchmarks/ntsc/http_fanout.nt: g
// goroutines each fetching one resource from the local server.
package main

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"strconv"
)

func fetch(port int, id int64, ch chan int) {
	resp, err := http.Get(fmt.Sprintf("http://127.0.0.1:%d/item?id=%d", port, id))
	if err != nil {
		ch <- 0
		return
	}
	body, err := io.ReadAll(resp.Body)
	closeErr := resp.Body.Close()
	if err != nil || closeErr != nil || resp.StatusCode != http.StatusOK {
		ch <- 0
		return
	}
	ch <- len(body)
}

func main() {
	g := int64(64)
	port := 8080
	if v := os.Getenv("NTS_BENCH_N"); v != "" {
		g, _ = strconv.ParseInt(v, 10, 64)
	}
	if v := os.Getenv("NTS_BENCH_PORT"); v != "" {
		port, _ = strconv.Atoi(v)
	}
	ch := make(chan int, 512)
	for id := int64(0); id < g; id++ {
		go fetch(port, id, ch)
	}
	var total int
	for i := int64(0); i < g; i++ {
		total += <-ch
	}
	fmt.Println(total)
}
