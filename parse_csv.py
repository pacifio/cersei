import sys
import csv
import json

def main():
    reader = csv.DictReader(sys.stdin)
    data = list(reader)
    print(json.dumps(data))

if __name__ == '__main__':
    main()
