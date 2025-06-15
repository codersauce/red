#!/usr/bin/env node

/**
 * Test runner for Red editor plugins
 * 
 * Usage: node test-runner.js <plugin-file> <test-file>
 */

const { MockRedAPI } = require('./mock-red.js');
const fs = require('fs');
const path = require('path');
const { performance } = require('perf_hooks');

// ANSI color codes
const colors = {
  reset: '\x1b[0m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  gray: '\x1b[90m'
};

// Test context
class TestContext {
  constructor(name) {
    this.name = name;
    this.tests = [];
    this.beforeEach = null;
    this.afterEach = null;
    this.beforeAll = null;
    this.afterAll = null;
  }

  test(name, fn) {
    this.tests.push({ name, fn, status: 'pending' });
  }

  it(name, fn) {
    this.test(name, fn);
  }
}

// Global test registry
const testSuites = [];
let currentSuite = null;

// Test DSL
global.describe = function(name, fn) {
  const suite = new TestContext(name);
  const previousSuite = currentSuite;
  currentSuite = suite;
  testSuites.push(suite);
  fn();
  currentSuite = previousSuite;
};

global.test = function(name, fn) {
  if (!currentSuite) {
    // Create a default suite
    currentSuite = new TestContext('Default');
    testSuites.push(currentSuite);
  }
  currentSuite.test(name, fn);
};

global.it = global.test;

global.beforeEach = function(fn) {
  if (currentSuite) currentSuite.beforeEach = fn;
};

global.afterEach = function(fn) {
  if (currentSuite) currentSuite.afterEach = fn;
};

global.beforeAll = function(fn) {
  if (currentSuite) currentSuite.beforeAll = fn;
};

global.afterAll = function(fn) {
  if (currentSuite) currentSuite.afterAll = fn;
};

// Assertion library
global.expect = function(actual) {
  return {
    toBe(expected) {
      if (actual !== expected) {
        throw new Error(`Expected ${JSON.stringify(actual)} to be ${JSON.stringify(expected)}`);
      }
    },
    toEqual(expected) {
      if (JSON.stringify(actual) !== JSON.stringify(expected)) {
        throw new Error(`Expected ${JSON.stringify(actual)} to equal ${JSON.stringify(expected)}`);
      }
    },
    toContain(item) {
      if (Array.isArray(actual)) {
        if (!actual.includes(item)) {
          throw new Error(`Expected array to contain ${JSON.stringify(item)}`);
        }
      } else if (typeof actual === 'string') {
        if (!actual.includes(item)) {
          throw new Error(`Expected string to contain "${item}"`);
        }
      } else {
        throw new Error(`toContain can only be used with arrays or strings`);
      }
    },
    toHaveBeenCalled() {
      if (!actual || !actual._isMock) {
        throw new Error(`Expected a mock function`);
      }
      if (actual._calls.length === 0) {
        throw new Error(`Expected function to have been called`);
      }
    },
    toHaveBeenCalledWith(...args) {
      if (!actual || !actual._isMock) {
        throw new Error(`Expected a mock function`);
      }
      const found = actual._calls.some(call => 
        JSON.stringify(call) === JSON.stringify(args)
      );
      if (!found) {
        throw new Error(`Expected function to have been called with ${JSON.stringify(args)}`);
      }
    },
    toThrow(message) {
      let threw = false;
      let error = null;
      try {
        if (typeof actual === 'function') {
          actual();
        }
      } catch (e) {
        threw = true;
        error = e;
      }
      if (!threw) {
        throw new Error(`Expected function to throw`);
      }
      if (message && !error.message.includes(message)) {
        throw new Error(`Expected error message to contain "${message}" but got "${error.message}"`);
      }
    }
  };
};

// Mock function creator
global.jest = {
  fn(implementation) {
    const mockFn = (...args) => {
      mockFn._calls.push(args);
      if (mockFn._implementation) {
        return mockFn._implementation(...args);
      }
    };
    mockFn._isMock = true;
    mockFn._calls = [];
    mockFn._implementation = implementation;
    mockFn.mockImplementation = (fn) => {
      mockFn._implementation = fn;
      return mockFn;
    };
    mockFn.mockClear = () => {
      mockFn._calls = [];
    };
    return mockFn;
  }
};

// Run tests
async function runTests(pluginPath, testPath) {
  console.log(`${colors.blue}Red Editor Plugin Test Runner${colors.reset}\n`);
  
  // Load plugin
  const plugin = require(path.resolve(pluginPath));
  
  // Load test file
  require(path.resolve(testPath));
  
  let totalTests = 0;
  let passedTests = 0;
  let failedTests = 0;
  
  // Run all test suites
  for (const suite of testSuites) {
    console.log(`\n${colors.blue}${suite.name}${colors.reset}`);
    
    // Setup mock Red API for this suite
    const red = new MockRedAPI();
    
    // Activate plugin
    if (plugin.activate) {
      await plugin.activate(red);
    }
    
    // Run beforeAll
    if (suite.beforeAll) {
      await suite.beforeAll();
    }
    
    // Run tests
    for (const test of suite.tests) {
      totalTests++;
      
      // Reset mock state
      red.clearLogs();
      
      // Run beforeEach
      if (suite.beforeEach) {
        await suite.beforeEach();
      }
      
      // Run test
      const start = performance.now();
      try {
        await test.fn(red);
        const duration = performance.now() - start;
        console.log(`  ${colors.green}✓${colors.reset} ${test.name} ${colors.gray}(${duration.toFixed(0)}ms)${colors.reset}`);
        passedTests++;
      } catch (error) {
        const duration = performance.now() - start;
        console.log(`  ${colors.red}✗${colors.reset} ${test.name} ${colors.gray}(${duration.toFixed(0)}ms)${colors.reset}`);
        console.log(`    ${colors.red}${error.message}${colors.reset}`);
        if (error.stack) {
          const stackLines = error.stack.split('\n').slice(1, 3);
          stackLines.forEach(line => console.log(`    ${colors.gray}${line.trim()}${colors.reset}`));
        }
        failedTests++;
      }
      
      // Run afterEach
      if (suite.afterEach) {
        await suite.afterEach();
      }
    }
    
    // Run afterAll
    if (suite.afterAll) {
      await suite.afterAll();
    }
    
    // Deactivate plugin
    if (plugin.deactivate) {
      await plugin.deactivate(red);
    }
  }
  
  // Summary
  console.log(`\n${colors.blue}Summary:${colors.reset}`);
  console.log(`  Total: ${totalTests}`);
  console.log(`  ${colors.green}Passed: ${passedTests}${colors.reset}`);
  if (failedTests > 0) {
    console.log(`  ${colors.red}Failed: ${failedTests}${colors.reset}`);
  }
  
  // Exit code
  process.exit(failedTests > 0 ? 1 : 0);
}

// Main
if (require.main === module) {
  const args = process.argv.slice(2);
  if (args.length < 2) {
    console.error('Usage: node test-runner.js <plugin-file> <test-file>');
    process.exit(1);
  }
  
  runTests(args[0], args[1]).catch(error => {
    console.error(`${colors.red}Test runner error:${colors.reset}`, error);
    process.exit(1);
  });
}