// Loop operations benchmark in Rust
// Matches neutron/benchmarks/neutron/loops.nt

fn sum_with_for_loop(n: i64) -> i64 {
    let mut sum = 0;
    let mut i = 0;
    while i < n {
        sum += i;
        i += 1;
    }
    sum
}

fn sum_with_while_loop(n: i64) -> i64 {
    let mut sum = 0;
    let mut i = 0;
    while i < n {
        sum += i;
        i += 1;
    }
    sum
}

fn nested_loops(depth: i64, size: i64) -> i64 {
    let mut count = 0;
    let mut i = 0;
    while i < depth {
        let mut j = 0;
        while j < size {
            let mut k = 0;
            while k < size {
                count += 1;
                k += 1;
            }
            j += 1;
        }
        i += 1;
    }
    count
}

fn main() {
    let s1 = sum_with_for_loop(100_000);
    let s2 = sum_with_while_loop(100_000);
    let n = nested_loops(10, 50);
    println!("{}", s1 + s2 + n);
}
