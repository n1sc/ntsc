CC ?= cc
AR ?= ar
CFLAGS ?= -O2 -Wall -Wextra -fPIC

all: libgeometry.a

geometry.o: geometry.c
	$(CC) $(CFLAGS) -c $< -o $@

libgeometry.a: geometry.o
	$(AR) rcs $@ $<

clean:
	rm -f geometry.o libgeometry.a

.PHONY: all clean
