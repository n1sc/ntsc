fn search(mask: i64, columns: i64, left: i64, right: i64) -> i64 {
    if columns == mask {
        return 1;
    }
    let mut available: i64 = mask & !(columns | left | right);
    let mut solutions: i64 = 0;
    while available != 0 {
        let bit: i64 = available & -available;
        available = available - bit;
        solutions = solutions
            + search(
                mask,
                columns | bit,
                ((left | bit) << 1) & mask,
                (right | bit) >> 1,
            );
    }
    return solutions;
}

fn queens(n: i64) -> f64 {
    return (search((1 << n) - 1, 0, 0, 0)) as f64;
}

fn mandelbrot(n: i64) -> f64 {
    let mut total: i64 = 0;
    let mut y: i64 = 0;
    while y < n {
        let cy: f64 = (y) as f64 * 2.4 / (n) as f64 - 1.2;
        let mut x: i64 = 0;
        while x < n {
            let cx: f64 = (x) as f64 * 3.2 / (n) as f64 - 2.2;
            let mut real: f64 = 0.0;
            let mut imag: f64 = 0.0;
            let mut iteration: i64 = 0;
            while iteration < 256 && real * real + imag * imag <= 4.0 {
                let next_real: f64 = real * real - imag * imag + cx;
                imag = 2.0 * real * imag + cy;
                real = next_real;
                iteration = iteration + 1;
            }
            total = total + iteration;
            x = x + 1;
        }
        y = y + 1;
    }
    return total as f64;
}

fn matmul(n: i64) -> f64 {
    let mut a = vec![0.0_f64; (n * n) as usize];
    let mut b = vec![0.0_f64; (n * n) as usize];
    let mut c = vec![0.0_f64; (n * n) as usize];
    let mut i: i64 = 0;
    while i < n * n {
        a[(i) as usize] = (((i * 17 + 3) % 97) as f64) / 97.0;
        b[(i) as usize] = (((i * 7 + 5) % 89) as f64) / 89.0;
        i = i + 1;
    }
    let mut row_block: i64 = 0;
    while row_block < n {
        let mut k_block: i64 = 0;
        while k_block < n {
            let mut col_block: i64 = 0;
            while col_block < n {
                let mut row_end: i64 = row_block + 32;
                if row_end > n {
                    row_end = n;
                }
                let mut k_end: i64 = k_block + 32;
                if k_end > n {
                    k_end = n;
                }
                let mut col_end: i64 = col_block + 32;
                if col_end > n {
                    col_end = n;
                }
                let mut row: i64 = row_block;
                while row < row_end {
                    let mut k: i64 = k_block;
                    while k < k_end {
                        let value: f64 = a[(row * n + k) as usize];
                        let mut col: i64 = col_block;
                        while col < col_end {
                            c[(row * n + col) as usize] =
                                c[(row * n + col) as usize] + value * b[(k * n + col) as usize];
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
    let mut total: f64 = 0.0;
    i = 0;
    while i < n * n {
        total = total + c[(i) as usize];
        i = i + 1;
    }
    return total;
}

fn radix(n: i64) -> f64 {
    let mut a = vec![0_i64; (n) as usize];
    let mut b = vec![0_i64; (n) as usize];
    let mut counts = vec![0_i64; (256) as usize];
    let mut state: i64 = 123456789;
    let mut i: i64 = 0;
    while i < n {
        state = (state * 1664525 + 1013904223) & 2147483647;
        a[(i) as usize] = state;
        i = i + 1;
    }
    let mut shift: i64 = 0;
    while shift < 32 {
        i = 0;
        while i < 256 {
            counts[(i) as usize] = 0;
            i = i + 1;
        }
        i = 0;
        while i < n {
            let bucket: i64 = (a[(i) as usize] >> shift) & 255;
            counts[(bucket) as usize] = counts[(bucket) as usize] + 1;
            i = i + 1;
        }
        let mut offset: i64 = 0;
        i = 0;
        while i < 256 {
            let count: i64 = counts[(i) as usize];
            counts[(i) as usize] = offset;
            offset = offset + count;
            i = i + 1;
        }
        i = 0;
        while i < n {
            let value: i64 = a[(i) as usize];
            let bucket: i64 = (value >> shift) & 255;
            let position: i64 = counts[(bucket) as usize];
            b[(position) as usize] = value;
            counts[(bucket) as usize] = position + 1;
            i = i + 1;
        }
        i = 0;
        while i < n {
            a[(i) as usize] = b[(i) as usize];
            i = i + 1;
        }
        shift = shift + 8;
    }
    let mut checksum: i64 = 0;
    i = 0;
    while i < n {
        if i > 0 && a[(i - 1) as usize] > a[(i) as usize] {
            return -1.0;
        }
        checksum = (checksum + a[(i) as usize] * (i + 1)) % 1000000007;
        i = i + 1;
    }
    return (checksum) as f64;
}

fn dijkstra(n: i64) -> f64 {
    let mut destinations = vec![0_i64; (n * 8) as usize];
    let mut weights = vec![0_i64; (n * 8) as usize];
    let mut distances = vec![0_i64; (n) as usize];
    let mut heap_nodes = vec![0_i64; (n * 8 + 1) as usize];
    let mut heap_costs = vec![0_i64; (n * 8 + 1) as usize];
    let mut state: i64 = 123456789;
    let mut vertex: i64 = 0;
    while vertex < n {
        distances[(vertex) as usize] = 1000000000000;
        let mut edge: i64 = 0;
        while edge < 8 {
            state = (state * 1664525 + 1013904223) & 2147483647;
            let mut destination: i64 = state % n;
            if edge == 0 {
                destination = (vertex + 1) % n;
            }
            destinations[(vertex * 8 + edge) as usize] = destination;
            weights[(vertex * 8 + edge) as usize] = 1 + ((state >> 8) & 31);
            edge = edge + 1;
        }
        vertex = vertex + 1;
    }
    distances[(0) as usize] = 0;
    let mut heap_size: i64 = 1;
    heap_nodes[(0) as usize] = 0;
    heap_costs[(0) as usize] = 0;
    while heap_size > 0 {
        let node: i64 = heap_nodes[(0) as usize];
        let cost: i64 = heap_costs[(0) as usize];
        heap_size = heap_size - 1;
        if heap_size > 0 {
            let last_node: i64 = heap_nodes[(heap_size) as usize];
            let last_cost: i64 = heap_costs[(heap_size) as usize];
            let mut position: i64 = 0;
            while position * 2 + 1 < heap_size {
                let mut child: i64 = position * 2 + 1;
                if child + 1 < heap_size
                    && heap_costs[(child + 1) as usize] < heap_costs[(child) as usize]
                {
                    child = child + 1;
                }
                if last_cost <= heap_costs[(child) as usize] {
                    break;
                }
                heap_nodes[(position) as usize] = heap_nodes[(child) as usize];
                heap_costs[(position) as usize] = heap_costs[(child) as usize];
                position = child;
            }
            heap_nodes[(position) as usize] = last_node;
            heap_costs[(position) as usize] = last_cost;
        }
        if cost == distances[(node) as usize] {
            let mut edge: i64 = 0;
            while edge < 8 {
                let destination: i64 = destinations[(node * 8 + edge) as usize];
                let candidate: i64 = cost + weights[(node * 8 + edge) as usize];
                if candidate < distances[(destination) as usize] {
                    distances[(destination) as usize] = candidate;
                    let mut position: i64 = heap_size;
                    heap_size = heap_size + 1;
                    while position > 0 {
                        let parent: i64 = (position - 1) / 2;
                        if heap_costs[(parent) as usize] <= candidate {
                            break;
                        }
                        heap_nodes[(position) as usize] = heap_nodes[(parent) as usize];
                        heap_costs[(position) as usize] = heap_costs[(parent) as usize];
                        position = parent;
                    }
                    heap_nodes[(position) as usize] = destination;
                    heap_costs[(position) as usize] = candidate;
                }
                edge = edge + 1;
            }
        }
    }
    let mut checksum: i64 = 0;
    vertex = 0;
    while vertex < n {
        checksum = (checksum + distances[(vertex) as usize] * (vertex + 1)) % 1000000007;
        vertex = vertex + 1;
    }
    return (checksum) as f64;
}

fn fft(n: i64) -> f64 {
    let mut real = vec![0.0_f64; (n) as usize];
    let mut imag = vec![0.0_f64; (n) as usize];
    let mut i: i64 = 0;
    while i < n {
        real[(i) as usize] = (((i * 17 + 3) % 97) as f64) / 97.0 - 0.5;
        i = i + 1;
    }
    let mut reversed: i64 = 0;
    i = 1;
    while i < n {
        let mut bit: i64 = n >> 1;
        while (reversed & bit) != 0 {
            reversed = reversed ^ bit;
            bit = bit >> 1;
        }
        reversed = reversed ^ bit;
        if i < reversed {
            let value: f64 = real[(i) as usize];
            real[(i) as usize] = real[(reversed) as usize];
            real[(reversed) as usize] = value;
        }
        i = i + 1;
    }
    let mut width: i64 = 2;
    while width <= n {
        let angle: f64 = -6.283185307179586 / (width) as f64;
        let root_real: f64 = angle.cos();
        let root_imag: f64 = angle.sin();
        let mut base: i64 = 0;
        while base < n {
            let mut twiddle_real: f64 = 1.0;
            let mut twiddle_imag: f64 = 0.0;
            let mut k: i64 = 0;
            while k < width / 2 {
                let first: i64 = base + k;
                let second: i64 = first + width / 2;
                let product_real: f64 =
                    twiddle_real * real[(second) as usize] - twiddle_imag * imag[(second) as usize];
                let product_imag: f64 =
                    twiddle_real * imag[(second) as usize] + twiddle_imag * real[(second) as usize];
                let first_real: f64 = real[(first) as usize];
                let first_imag: f64 = imag[(first) as usize];
                real[(first) as usize] = first_real + product_real;
                imag[(first) as usize] = first_imag + product_imag;
                real[(second) as usize] = first_real - product_real;
                imag[(second) as usize] = first_imag - product_imag;
                let next_real: f64 = twiddle_real * root_real - twiddle_imag * root_imag;
                twiddle_imag = twiddle_real * root_imag + twiddle_imag * root_real;
                twiddle_real = next_real;
                k = k + 1;
            }
            base = base + width;
        }
        width = width * 2;
    }
    let mut energy: f64 = 0.0;
    i = 0;
    while i < n {
        energy = energy
            + real[(i) as usize] * real[(i) as usize]
            + imag[(i) as usize] * imag[(i) as usize];
        i = i + 1;
    }
    return energy;
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let invalid = |message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message);
    if args.len() != 3 {
        return Err(invalid("usage: kernels WORKLOAD SIZE").into());
    }
    let n: i64 = args[2].parse()?;
    if !(1..=4_000_000).contains(&n) {
        return Err(invalid("invalid size").into());
    }
    let result = match args[1].as_str() {
        "queens" if n <= 20 => queens(n),
        "mandelbrot" => mandelbrot(n),
        "matmul" if n <= 1024 => matmul(n),
        "radix" => radix(n),
        "dijkstra" => dijkstra(n),
        "fft" if n & (n - 1) == 0 => fft(n),
        _ => return Err(invalid("unknown workload or invalid size").into()),
    };
    println!("{result}");
    Ok(())
}
