package main

import (
	"fmt"
	"math"
	"os"
	"strconv"
)

func search(mask int64, columns int64, left int64, right int64) int64 {
	if columns == mask {
		return 1
	}
	var available int64 = mask & ^(columns | left | right)
	var solutions int64 = 0
	for available != 0 {
		var bit int64 = available & -available
		available = available - bit
		solutions = solutions + search(mask, columns|bit, ((left|bit)<<1)&mask, (right|bit)>>1)
	}
	return solutions
}

func queens(n int64) float64 {
	return float64(search((1<<n)-1, 0, 0, 0))
}

func mandelbrot(n int64) float64 {
	var total int64 = 0
	var y int64 = 0
	for y < n {
		var cy float64 = float64(y)*2.4/float64(n) - 1.2
		var x int64 = 0
		for x < n {
			var cx float64 = float64(x)*3.2/float64(n) - 2.2
			var real float64 = 0.0
			var imag float64 = 0.0
			var iteration int64 = 0
			for iteration < 256 && real*real+imag*imag <= 4.0 {
				var next_real float64 = real*real - imag*imag + cx
				imag = 2.0*real*imag + cy
				real = next_real
				iteration = iteration + 1
			}
			total = total + iteration
			x = x + 1
		}
		y = y + 1
	}
	return float64(total)
}

func matmul(n int64) float64 {
	var a = make([]float64, int((n * n)))
	var b = make([]float64, int((n * n)))
	var c = make([]float64, int((n * n)))
	var i int64 = 0
	for i < n*n {
		a[i] = (float64((i*17 + 3) % 97)) / 97.0
		b[i] = (float64((i*7 + 5) % 89)) / 89.0
		i = i + 1
	}
	var row_block int64 = 0
	for row_block < n {
		var k_block int64 = 0
		for k_block < n {
			var col_block int64 = 0
			for col_block < n {
				var row_end int64 = row_block + 32
				if row_end > n {
					row_end = n
				}
				var k_end int64 = k_block + 32
				if k_end > n {
					k_end = n
				}
				var col_end int64 = col_block + 32
				if col_end > n {
					col_end = n
				}
				var row int64 = row_block
				for row < row_end {
					var k int64 = k_block
					for k < k_end {
						var value float64 = a[row*n+k]
						var col int64 = col_block
						for col < col_end {
							c[row*n+col] = c[row*n+col] + value*b[k*n+col]
							col = col + 1
						}
						k = k + 1
					}
					row = row + 1
				}
				col_block = col_block + 32
			}
			k_block = k_block + 32
		}
		row_block = row_block + 32
	}
	var total float64 = 0.0
	i = 0
	for i < n*n {
		total = total + c[i]
		i = i + 1
	}
	return total
}

func radix(n int64) float64 {
	var a = make([]int64, int(n))
	var b = make([]int64, int(n))
	var counts = make([]int64, int(256))
	var state int64 = 123456789
	var i int64 = 0
	for i < n {
		state = (state*1664525 + 1013904223) & 2147483647
		a[i] = state
		i = i + 1
	}
	var shift int64 = 0
	for shift < 32 {
		i = 0
		for i < 256 {
			counts[i] = 0
			i = i + 1
		}
		i = 0
		for i < n {
			var bucket int64 = (a[i] >> shift) & 255
			counts[bucket] = counts[bucket] + 1
			i = i + 1
		}
		var offset int64 = 0
		i = 0
		for i < 256 {
			var count int64 = counts[i]
			counts[i] = offset
			offset = offset + count
			i = i + 1
		}
		i = 0
		for i < n {
			var value int64 = a[i]
			var bucket int64 = (value >> shift) & 255
			var position int64 = counts[bucket]
			b[position] = value
			counts[bucket] = position + 1
			i = i + 1
		}
		i = 0
		for i < n {
			a[i] = b[i]
			i = i + 1
		}
		shift = shift + 8
	}
	var checksum int64 = 0
	i = 0
	for i < n {
		if i > 0 && a[i-1] > a[i] {
			return -1.0
		}
		checksum = (checksum + a[i]*(i+1)) % 1000000007
		i = i + 1
	}
	return float64(checksum)
}

func dijkstra(n int64) float64 {
	var destinations = make([]int64, int((n * 8)))
	var weights = make([]int64, int((n * 8)))
	var distances = make([]int64, int(n))
	var heap_nodes = make([]int64, int((n*8 + 1)))
	var heap_costs = make([]int64, int((n*8 + 1)))
	var state int64 = 123456789
	var vertex int64 = 0
	for vertex < n {
		distances[vertex] = 1000000000000
		var edge int64 = 0
		for edge < 8 {
			state = (state*1664525 + 1013904223) & 2147483647
			var destination int64 = state % n
			if edge == 0 {
				destination = (vertex + 1) % n
			}
			destinations[vertex*8+edge] = destination
			weights[vertex*8+edge] = 1 + ((state >> 8) & 31)
			edge = edge + 1
		}
		vertex = vertex + 1
	}
	distances[0] = 0
	var heap_size int64 = 1
	heap_nodes[0] = 0
	heap_costs[0] = 0
	for heap_size > 0 {
		var node int64 = heap_nodes[0]
		var cost int64 = heap_costs[0]
		heap_size = heap_size - 1
		if heap_size > 0 {
			var last_node int64 = heap_nodes[heap_size]
			var last_cost int64 = heap_costs[heap_size]
			var position int64 = 0
			for position*2+1 < heap_size {
				var child int64 = position*2 + 1
				if child+1 < heap_size && heap_costs[child+1] < heap_costs[child] {
					child = child + 1
				}
				if last_cost <= heap_costs[child] {
					break
				}
				heap_nodes[position] = heap_nodes[child]
				heap_costs[position] = heap_costs[child]
				position = child
			}
			heap_nodes[position] = last_node
			heap_costs[position] = last_cost
		}
		if cost == distances[node] {
			var edge int64 = 0
			for edge < 8 {
				var destination int64 = destinations[node*8+edge]
				var candidate int64 = cost + weights[node*8+edge]
				if candidate < distances[destination] {
					distances[destination] = candidate
					var position int64 = heap_size
					heap_size = heap_size + 1
					for position > 0 {
						var parent int64 = (position - 1) / 2
						if heap_costs[parent] <= candidate {
							break
						}
						heap_nodes[position] = heap_nodes[parent]
						heap_costs[position] = heap_costs[parent]
						position = parent
					}
					heap_nodes[position] = destination
					heap_costs[position] = candidate
				}
				edge = edge + 1
			}
		}
	}
	var checksum int64 = 0
	vertex = 0
	for vertex < n {
		checksum = (checksum + distances[vertex]*(vertex+1)) % 1000000007
		vertex = vertex + 1
	}
	return float64(checksum)
}

func fft(n int64) float64 {
	var real = make([]float64, int(n))
	var imag = make([]float64, int(n))
	var i int64 = 0
	for i < n {
		real[i] = (float64((i*17+3)%97))/97.0 - 0.5
		i = i + 1
	}
	var reversed int64 = 0
	i = 1
	for i < n {
		var bit int64 = n >> 1
		for (reversed & bit) != 0 {
			reversed = reversed ^ bit
			bit = bit >> 1
		}
		reversed = reversed ^ bit
		if i < reversed {
			var value float64 = real[i]
			real[i] = real[reversed]
			real[reversed] = value
		}
		i = i + 1
	}
	var width int64 = 2
	for width <= n {
		var angle float64 = -6.283185307179586 / float64(width)
		var root_real float64 = math.Cos(angle)
		var root_imag float64 = math.Sin(angle)
		var base int64 = 0
		for base < n {
			var twiddle_real float64 = 1.0
			var twiddle_imag float64 = 0.0
			var k int64 = 0
			for k < width/2 {
				var first int64 = base + k
				var second int64 = first + width/2
				var product_real float64 = twiddle_real*real[second] - twiddle_imag*imag[second]
				var product_imag float64 = twiddle_real*imag[second] + twiddle_imag*real[second]
				var first_real float64 = real[first]
				var first_imag float64 = imag[first]
				real[first] = first_real + product_real
				imag[first] = first_imag + product_imag
				real[second] = first_real - product_real
				imag[second] = first_imag - product_imag
				var next_real float64 = twiddle_real*root_real - twiddle_imag*root_imag
				twiddle_imag = twiddle_real*root_imag + twiddle_imag*root_real
				twiddle_real = next_real
				k = k + 1
			}
			base = base + width
		}
		width = width * 2
	}
	var energy float64 = 0.0
	i = 0
	for i < n {
		energy = energy + real[i]*real[i] + imag[i]*imag[i]
		i = i + 1
	}
	return energy
}

func run() error {
	if len(os.Args) != 3 {
		return fmt.Errorf("usage: kernels WORKLOAD SIZE")
	}
	n, err := strconv.ParseInt(os.Args[2], 10, 64)
	if err != nil {
		return err
	}
	if n < 1 || n > 4000000 {
		return fmt.Errorf("invalid size")
	}
	var result float64
	switch {
	case os.Args[1] == "queens" && n <= 20:
		result = queens(n)
	case os.Args[1] == "mandelbrot":
		result = mandelbrot(n)
	case os.Args[1] == "matmul" && n <= 1024:
		result = matmul(n)
	case os.Args[1] == "radix":
		result = radix(n)
	case os.Args[1] == "dijkstra":
		result = dijkstra(n)
	case os.Args[1] == "fft" && n&(n-1) == 0:
		result = fft(n)
	default:
		return fmt.Errorf("unknown workload or invalid size")
	}
	fmt.Printf("%.15g\n", result)
	return nil
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
}
