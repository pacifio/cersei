import sys

def gcd(a, b):
    a = abs(a)
    b = abs(b)
    while b != 0:
        a, b = b, a % b
    return a

if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(1)
    
    num1 = int(sys.argv[1])
    num2 = int(sys.argv[2])
    
    print(gcd(num1, num2))
