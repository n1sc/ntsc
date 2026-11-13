#include <inttypes.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int64_t search(int64_t mask, int64_t columns, int64_t left, int64_t right) {
    if (columns == mask) {
        return 1;
    }
    int64_t available = mask & ~(columns | left | right);
    int64_t solutions = 0;
    while (available != 0) {
        int64_t bit = available & -available;
        available = available - bit;
        solutions = solutions + search(mask, columns | bit, ((left | bit) << 1) & mask, (right | bit) >> 1);
    }
    return solutions;
}

static double queens(int64_t n) {
    return search((1LL << n) - 1, 0, 0, 0);
}

static double mandelbrot(int64_t n) {
    int64_t total = 0;
    int64_t y = 0;
    while (y < n) {
        double cy = y * 2.4 / n - 1.2;
        int64_t x = 0;
        while (x < n) {
            double cx = x * 3.2 / n - 2.2;
            double real = 0.0;
            double imag = 0.0;
            int64_t iteration = 0;
            while (iteration < 256 && real * real + imag * imag <= 4.0) {
                double next_real = real * real - imag * imag + cx;
                imag = 2.0 * real * imag + cy;
                real = next_real;
                iteration = iteration + 1;
            }
            total = total + iteration;
            x = x + 1;
        }
        y = y + 1;
    }
    return total;
}

static double matmul(int64_t n) {
    double *a = calloc((size_t)(n * n), sizeof(double));
    double *b = calloc((size_t)(n * n), sizeof(double));
    double *c = calloc((size_t)(n * n), sizeof(double));
    if (!a || !b || !c) { fprintf(stderr, "allocation failed\n"); exit(2); }
    int64_t i = 0;
    while (i < n * n) {
        a[i] = ((i * 17 + 3) % 97) / 97.0;
        b[i] = ((i * 7 + 5) % 89) / 89.0;
        i = i + 1;
    }
    int64_t row_block = 0;
    while (row_block < n) {
        int64_t k_block = 0;
        while (k_block < n) {
            int64_t col_block = 0;
            while (col_block < n) {
                int64_t row_end = row_block + 32;
                if (row_end > n) { row_end = n; }
                int64_t k_end = k_block + 32;
                if (k_end > n) { k_end = n; }
                int64_t col_end = col_block + 32;
                if (col_end > n) { col_end = n; }
                int64_t row = row_block;
                while (row < row_end) {
                    int64_t k = k_block;
                    while (k < k_end) {
                        double value = a[row * n + k];
                        int64_t col = col_block;
                        while (col < col_end) {
                            c[row * n + col] = c[row * n + col] + value * b[k * n + col];
                            col = col + 1;
                        }
                        k = k + 1;
                    }
                    row = row + 1;
                }
                col_block = col_block + 32;
            }
            k_block = k_block + 32;
        }
        row_block = row_block + 32;
    }
    double total = 0.0;
    i = 0;
    while (i < n * n) {
        total = total + c[i];
        i = i + 1;
    }
    free(a); free(b); free(c);
    return total;
}

static double radix(int64_t n) {
    int64_t *a = calloc((size_t)n, sizeof(int64_t));
    int64_t *b = calloc((size_t)n, sizeof(int64_t));
    int64_t *counts = calloc(256, sizeof(int64_t));
    if (!a || !b || !counts) { fprintf(stderr, "allocation failed\n"); exit(2); }
    int64_t state = 123456789;
    int64_t i = 0;
    while (i < n) {
        state = (state * 1664525 + 1013904223) & 2147483647;
        a[i] = state;
        i = i + 1;
    }
    int64_t shift = 0;
    while (shift < 32) {
        i = 0;
        while (i < 256) {
            counts[i] = 0;
            i = i + 1;
        }
        i = 0;
        while (i < n) {
            int64_t bucket = (a[i] >> shift) & 255;
            counts[bucket] = counts[bucket] + 1;
            i = i + 1;
        }
        int64_t offset = 0;
        i = 0;
        while (i < 256) {
            int64_t count = counts[i];
            counts[i] = offset;
            offset = offset + count;
            i = i + 1;
        }
        i = 0;
        while (i < n) {
            int64_t value = a[i];
            int64_t bucket = (value >> shift) & 255;
            int64_t position = counts[bucket];
            b[position] = value;
            counts[bucket] = position + 1;
            i = i + 1;
        }
        i = 0;
        while (i < n) {
            a[i] = b[i];
            i = i + 1;
        }
        shift = shift + 8;
    }
    int64_t checksum = 0;
    i = 0;
    while (i < n) {
        if (i > 0 && a[i - 1] > a[i]) { return -1; }
        checksum = (checksum + a[i] * (i + 1)) % 1000000007;
        i = i + 1;
    }
    free(a); free(b); free(counts);
    return checksum;
}

static double dijkstra(int64_t n) {
    int64_t *destinations = calloc((size_t)(n * 8), sizeof(int64_t));
    int64_t *weights = calloc((size_t)(n * 8), sizeof(int64_t));
    int64_t *distances = calloc((size_t)n, sizeof(int64_t));
    int64_t *heap_nodes = calloc((size_t)(n * 8 + 1), sizeof(int64_t));
    int64_t *heap_costs = calloc((size_t)(n * 8 + 1), sizeof(int64_t));
    if (!destinations || !weights || !distances || !heap_nodes || !heap_costs) {
        fprintf(stderr, "allocation failed\n"); exit(2);
    }
    int64_t state = 123456789;
    int64_t vertex = 0;
    while (vertex < n) {
        distances[vertex] = 1000000000000;
        int64_t edge = 0;
        while (edge < 8) {
            state = (state * 1664525 + 1013904223) & 2147483647;
            int64_t destination = state % n;
            if (edge == 0) { destination = (vertex + 1) % n; }
            destinations[vertex * 8 + edge] = destination;
            weights[vertex * 8 + edge] = 1 + ((state >> 8) & 31);
            edge = edge + 1;
        }
        vertex = vertex + 1;
    }
    distances[0] = 0;
    int64_t heap_size = 1;
    heap_nodes[0] = 0;
    heap_costs[0] = 0;
    while (heap_size > 0) {
        int64_t node = heap_nodes[0];
        int64_t cost = heap_costs[0];
        heap_size = heap_size - 1;
        if (heap_size > 0) {
            int64_t last_node = heap_nodes[heap_size];
            int64_t last_cost = heap_costs[heap_size];
            int64_t position = 0;
            while (position * 2 + 1 < heap_size) {
                int64_t child = position * 2 + 1;
                if (child + 1 < heap_size && heap_costs[child + 1] < heap_costs[child]) {
                    child = child + 1;
                }
                if (last_cost <= heap_costs[child]) { break; }
                heap_nodes[position] = heap_nodes[child];
                heap_costs[position] = heap_costs[child];
                position = child;
            }
            heap_nodes[position] = last_node;
            heap_costs[position] = last_cost;
        }
        if (cost == distances[node]) {
            int64_t edge = 0;
            while (edge < 8) {
                int64_t destination = destinations[node * 8 + edge];
                int64_t candidate = cost + weights[node * 8 + edge];
                if (candidate < distances[destination]) {
                    distances[destination] = candidate;
                    int64_t position = heap_size;
                    heap_size = heap_size + 1;
                    while (position > 0) {
                        int64_t parent = (position - 1) / 2;
                        if (heap_costs[parent] <= candidate) { break; }
                        heap_nodes[position] = heap_nodes[parent];
                        heap_costs[position] = heap_costs[parent];
                        position = parent;
                    }
                    heap_nodes[position] = destination;
                    heap_costs[position] = candidate;
                }
                edge = edge + 1;
            }
        }
    }
    int64_t checksum = 0;
    vertex = 0;
    while (vertex < n) {
        checksum = (checksum + distances[vertex] * (vertex + 1)) % 1000000007;
        vertex = vertex + 1;
    }
    free(destinations); free(weights); free(distances); free(heap_nodes); free(heap_costs);
    return checksum;
}

static double fft(int64_t n) {
    double *real = calloc((size_t)n, sizeof(double));
    double *imag = calloc((size_t)n, sizeof(double));
    if (!real || !imag) { fprintf(stderr, "allocation failed\n"); exit(2); }
    int64_t i = 0;
    while (i < n) {
        real[i] = ((i * 17 + 3) % 97) / 97.0 - 0.5;
        i = i + 1;
    }
    int64_t reversed = 0;
    i = 1;
    while (i < n) {
        int64_t bit = n >> 1;
        while ((reversed & bit) != 0) {
            reversed = reversed ^ bit;
            bit = bit >> 1;
        }
        reversed = reversed ^ bit;
        if (i < reversed) {
            double value = real[i];
            real[i] = real[reversed];
            real[reversed] = value;
        }
        i = i + 1;
    }
    int64_t width = 2;
    while (width <= n) {
        double angle = -6.283185307179586 / width;
        double root_real = cos(angle);
        double root_imag = sin(angle);
        int64_t base = 0;
        while (base < n) {
            double twiddle_real = 1.0;
            double twiddle_imag = 0.0;
            int64_t k = 0;
            while (k < width / 2) {
                int64_t first = base + k;
                int64_t second = first + width / 2;
                double product_real = twiddle_real * real[second] - twiddle_imag * imag[second];
                double product_imag = twiddle_real * imag[second] + twiddle_imag * real[second];
                double first_real = real[first];
                double first_imag = imag[first];
                real[first] = first_real + product_real;
                imag[first] = first_imag + product_imag;
                real[second] = first_real - product_real;
                imag[second] = first_imag - product_imag;
                double next_real = twiddle_real * root_real - twiddle_imag * root_imag;
                twiddle_imag = twiddle_real * root_imag + twiddle_imag * root_real;
                twiddle_real = next_real;
                k = k + 1;
            }
            base = base + width;
        }
        width = width * 2;
    }
    double energy = 0.0;
    i = 0;
    while (i < n) {
        energy = energy + real[i] * real[i] + imag[i] * imag[i];
        i = i + 1;
    }
    free(real); free(imag);
    return energy;
}

int main(int argc, char **argv) {
    if (argc != 3) { fprintf(stderr, "usage: kernels WORKLOAD SIZE\n"); return 2; }
    char *end = NULL;
    int64_t n = strtoll(argv[2], &end, 10);
    if (*end != '\0' || n < 1 || n > 4000000) { fprintf(stderr, "invalid size\n"); return 2; }
    double result;
    if (strcmp(argv[1], "queens") == 0 && n <= 20) { result = queens(n); }
    else if (strcmp(argv[1], "mandelbrot") == 0) { result = mandelbrot(n); }
    else if (strcmp(argv[1], "matmul") == 0 && n <= 1024) { result = matmul(n); }
    else if (strcmp(argv[1], "radix") == 0) { result = radix(n); }
    else if (strcmp(argv[1], "dijkstra") == 0) { result = dijkstra(n); }
    else if (strcmp(argv[1], "fft") == 0 && (n & (n - 1)) == 0) { result = fft(n); }
    else { fprintf(stderr, "unknown workload or invalid size\n"); return 2; }
    printf("%.15g\n", result);
    return 0;
}
