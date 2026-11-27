"""Keep nextest timing and retry data without uploading captured process output."""

import sys
import xml.etree.ElementTree as ET


def timing_report(source):
    root = ET.parse(source).getroot()
    if root.tag != "testsuites":
        raise ValueError("expected a nextest testsuites report")

    attributes = {
        "name", "classname", "time", "timestamp", "uuid",
        "tests", "disabled", "skipped", "failures", "errors",
    }
    outcomes = {
        "failure", "error", "skipped", "flakyFailure", "flakyError",
        "rerunFailure", "rerunError",
    }
    children = {"testsuites": {"testsuite"}, "testsuite": {"testcase"}, "testcase": outcomes}

    def copy_timings(element):
        result = ET.Element(element.tag, {
            key: value for key, value in element.attrib.items() if key in attributes
        })
        for child in element:
            if child.tag in children.get(element.tag, ()):
                result.append(copy_timings(child))
        return result

    return ET.ElementTree(copy_timings(root))


if __name__ == "__main__":
    timing_report(sys.argv[1]).write(sys.argv[2], encoding="utf-8", xml_declaration=True)
