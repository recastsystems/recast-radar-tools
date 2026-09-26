// go-nexrad (github.com/bwiggs/go-nexrad/archive2) harness for
// tools/xlib-bench: each sample is os.ReadFile + archive2.Extract, which
// decompresses every LDM record and reads every message 31 moment into raw
// byte slices. Protocol: FORMAT FILE ITERS WARMUP [WAIT_STDIN].
package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strconv"
	"time"

	"github.com/bwiggs/go-nexrad/archive2"
)

// statusKB is a /proc/self/status value in KiB ("VmRSS:", "VmHWM:"), 0 if unknown.
func statusKB(key string) int64 {
	b, err := os.ReadFile("/proc/self/status")
	if err != nil {
		return 0
	}
	for _, line := range bytes.Split(b, []byte("\n")) {
		if bytes.HasPrefix(line, []byte(key)) {
			var kb int64
			fmt.Sscanf(string(line[len(key):]), "%d", &kb)
			return kb
		}
	}
	return 0
}

func rssKB() int64 { return statusKB("VmRSS:") }

func main() {
	if len(os.Args) < 5 {
		fmt.Fprintln(os.Stderr, "usage: go-nexrad-bench FORMAT FILE ITERS WARMUP [1]")
		os.Exit(2)
	}
	path := os.Args[2]
	iters, _ := strconv.Atoi(os.Args[3])
	warmup, _ := strconv.Atoi(os.Args[4])
	if len(os.Args) > 5 && os.Args[5] == "1" {
		bufio.NewReader(os.Stdin).ReadString('\n')
	}
	rssBefore := rssKB()
	samples := []float64{}
	sweeps, gates := 0, 0
	for i := 0; i < warmup+iters; i++ {
		t := time.Now()
		b, err := os.ReadFile(path)
		if err != nil {
			panic(err)
		}
		ar2 := archive2.Extract(bytes.NewReader(b))
		ms := float64(time.Since(t).Nanoseconds()) / 1e6
		if i >= warmup {
			samples = append(samples, ms)
		}
		sweeps = len(ar2.ElevationScans)
		gates = 0
		for _, scan := range ar2.ElevationScans {
			for _, m := range scan {
				for _, d := range []*archive2.DataMoment{m.ReflectivityData, m.VelocityData, m.SwData, m.ZdrData, m.PhiData, m.RhoData, m.CfpData} {
					if d != nil {
						gates += int(d.NumberDataMomentGates)
					}
				}
			}
		}
	}
	sorted := append([]float64(nil), samples...)
	sort.Float64s(sorted)
	out := map[string]any{
		"lib": "go-nexrad", "format": os.Args[1], "iters": iters,
		"median_ms": sorted[len(sorted)/2], "min_ms": sorted[0], "samples_ms": samples,
		"sweeps": sweeps, "fields": 0, "gates": gates, "rss_before_kb": rssBefore,
		"self_hwm_kb": statusKB("VmHWM:"),
	}
	enc, _ := json.Marshal(out)
	fmt.Println(string(enc))
}
