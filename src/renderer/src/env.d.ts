/// <reference types="vite/client" />
import type { PrimeApi } from '../../preload'
import type { JSX as ReactJSX } from 'react'

declare global {
  interface Window {
    prime: PrimeApi
  }
  namespace JSX {
    type Element = ReactJSX.Element
  }
}

export {}
