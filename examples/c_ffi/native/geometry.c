#include <math.h>

double geometry_distance(double x, double y) {
    return hypot(x, y);
}

double geometry_signal(double phase) {
    return sin(phase) + 0.5 * cos(phase * 2.0);
}
