static inline int square(int value) {
    return value * value;
}

static int sum_squares(int left, int right) {
    return square(left) + square(right);
}

int main(int argc, char **argv) {
    return sum_squares(argc, argv != 0);
}
